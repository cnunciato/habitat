use config::Config;
use data_store::DataStore;
use error::Result;
use hab_net::server::ZMQ_CONTEXT;
use protobuf::parse_from_bytes;
use protocol::jobsrv::{JobLogComplete, JobLogChunk};
use server::log::LogDirectory;
use std::fs::OpenOptions;
use std::io::Write;
use std::str;
use std::sync::{mpsc, Arc, RwLock};
use std::thread::{self, JoinHandle};
use zmq;

/// Listens for log messages from builders and consolidates output for
/// both streaming to clients and long-term storage.
pub struct LogIngester {
    intake_sock: zmq::Socket,
    config: Arc<RwLock<Config>>,
    msg: zmq::Message,
    log_dir: LogDirectory,
    data_store: DataStore
}

impl LogIngester {

    pub fn new(config: Arc<RwLock<Config>>,
               log_dir: LogDirectory,
               data_store: DataStore) -> Result<Self> {
        let intake_sock = (**ZMQ_CONTEXT).as_mut().socket(zmq::ROUTER)?;
        intake_sock.set_router_mandatory(true)?;
        let msg = zmq::Message::new()?;
        Ok(LogIngester {
            intake_sock: intake_sock,
            config: config,
            msg: msg,
            log_dir: log_dir,
            data_store: data_store
        })
    }

    pub fn start(cfg: Arc<RwLock<Config>>,
                 log_dir: LogDirectory,
                 data_store: DataStore) -> Result<JoinHandle<()>> {
        let (tx, rx) = mpsc::sync_channel(1);
        let handle = thread::Builder::new()
            .name("log-ingester".to_string())
            .spawn(move || {
                       let mut ingester = Self::new(cfg, log_dir, data_store).unwrap();
                       ingester.run(tx).unwrap();
                   })
            .unwrap();
        match rx.recv() {
            Ok(()) => Ok(handle),
            Err(e) => panic!("log-ingester thread startup error, err={}", e),
        }
    }

    fn run(&mut self, rz: mpsc::SyncSender<()>) -> Result<()> {
        {
            let cfg = self.config.read().unwrap();
            let addr = cfg.net.log_ingestion_addr();
            println!("Listening for log data on {}", addr);
            self.intake_sock.bind(&addr)?;
        }

        rz.send(()).unwrap();

        loop {
            // Right now we've got 3 frames per message:
            // 1: peer identity (we're using a ROUTER socket)
            // 2: a single-character code indicating message type:
            //    L = a line of log output
            //    C = the log is complete
            // 3: a protobuf message

            self.intake_sock.recv(&mut self.msg, 0)?; // identity frame

            match str::from_utf8(self.intake_sock.recv_bytes(0).unwrap().as_slice()).unwrap() {
                "L" => {
                    self.intake_sock.recv(&mut self.msg, 0)?; // protobuf message frame
                    match parse_from_bytes::<JobLogChunk>(&self.msg) {
                        Ok(chunk) => {
                            let log_file = self.log_dir.log_file_path(chunk.get_job_id());

                            // TODO: Consider caching file handles for
                            // currently-processing logs.
                            let open = OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(log_file.as_path());

                            match open {
                                Ok(mut file) => {
                                    // TODO: Just write the chunk body; line number is
                                    // for debugging only
                                    file.write(format!("{}: {}",
                                                       chunk.get_seq(),
                                                       chunk.get_body()).as_bytes())?;
                                    file.flush()?;
                                    println!("LOG: {:?}: {:?}",
                                             chunk.get_seq(),
                                             chunk.get_body());
                                },
                                Err(e) => {
                                    warn!("Could not open {:?} for appending! {:?}", log_file, e);
                                }
                            }
                        },
                        Err(e) => {
                            warn!("ERROR parsing JobLogChunk: {:?}", e);
                        }
                    }
                },
                "C" => {
                    self.intake_sock.recv(&mut self.msg, 0)?; // protobuf message frame
                    match parse_from_bytes::<JobLogComplete>(&self.msg) {
                        Ok(complete) => {
                            let id = complete.get_job_id();
                            debug!("Log complete for job {:?}", id);

                            // TODO: Save logs in long-term storage,
                            // like S3
                            let log_file = self.log_dir.log_file_path(id);

                            // Until we can ship things off to an
                            // S3-alike, we'll just use a file URL
                            let url = format!("file://{}", log_file.to_str().unwrap());
                            self.data_store.set_log_url(id, &url)?;
                        },
                        Err(e) => {
                            warn!("ERROR parsing JobLogComplete: {:?}", e);
                        }
                    }
                },
                other => {
                    warn!("UNRECOGNIZED LOG PROTOCOL CODE: {:?}", other);
                }
            }
        }
    }
}
