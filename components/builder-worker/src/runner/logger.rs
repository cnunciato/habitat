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

use hab_net::server::{ZMQ_CONTEXT};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::process;
use zmq;
use protobuf::Message;
use protocol::jobsrv::{JobLogComplete, JobLogChunk};
use super::workspace::Workspace;

const INPROC_ADDR: &'static str = "inproc://logger";
const EOL_MARKER: &'static str = "\n";

pub struct Logger {
    job_id: u64,
    stdout: File,
    stderr: File,
    sock: zmq::Socket
}

impl Logger {
    pub fn init(workspace: &Workspace) -> Self {
        let stdout = workspace.root().join("stdout.log");
        let stderr = workspace.root().join("stderr.log");
        let sock = (**ZMQ_CONTEXT).as_mut().socket(zmq::PUSH).unwrap();
        sock.set_immediate(true).unwrap();
        sock.set_linger(5000).unwrap();
        sock.connect(INPROC_ADDR).unwrap();

        Logger {
            job_id: workspace.job.get_id(),
            stdout: File::create(stdout).expect("Failed to initialize stdout log file"),
            stderr: File::create(stderr).expect("Failed to initialize stderr log file"),
            sock: sock
        }
    }

    /// Stream stdout and stderr of the given child process into the appropriate log files
    pub fn pipe(&mut self, process: &mut process::Child) {
        if let Some(ref mut stdout) = process.stdout {

            let mut line_num = 0;
            for line in BufReader::new(stdout).lines() {
                line_num = line_num + 1;
                let mut l: String = line.unwrap();
                l = l + EOL_MARKER;

                let mut chunk = JobLogChunk::new();
                chunk.set_job_id(self.job_id);
                chunk.set_seq(line_num);
                chunk.set_body(l.clone());

                // L = "log"
                self.sock.send(b"L", zmq::SNDMORE).unwrap();
                self.sock.send(chunk.write_to_bytes().unwrap()
                               .as_slice(), 0).unwrap();

                self.log_stdout(l.as_bytes());
            }

            // Signal that the log is finished
            let mut complete = JobLogComplete::new();
            complete.set_job_id(self.job_id);

            // C = "log complete"
            self.sock.send(b"C", zmq::SNDMORE).unwrap();
            self.sock.send(complete.write_to_bytes().unwrap().as_slice(), 0).unwrap();

        }
        if let Some(ref mut stderr) = process.stderr {
            let mut line_num = 0;
            for line in BufReader::new(stderr).lines() {
                let mut l: String = line.unwrap();
                line_num = line_num + 1;
                l = l + EOL_MARKER;
                self.log_stderr(l.as_bytes());
            }
        }
    }

    /// Log message to stdout logfile
    pub fn log_stdout(&mut self, msg: &[u8]) {
        self.stdout
            .write_all(msg)
            .expect(&format!("Logger unable to write to {:?}", self.stdout));
    }

    /// Log message to stderr logfile
    pub fn log_stderr(&mut self, msg: &[u8]) {
        self.stderr
            .write_all(msg)
            .expect(&format!("Logger unable to write to {:?}", self.stderr))
    }
}

impl Drop for Logger {
    fn drop(&mut self) {
        self.stdout
            .sync_all()
            .expect("Unable to sync stdout log file");
        self.stderr
            .sync_all()
            .expect("Unable to sync stderr log file");
    }
}
