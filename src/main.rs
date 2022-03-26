#[macro_use]
extern crate log;
#[macro_use]
extern crate serde;

use futures_util::future::FutureExt;
use tokio::io::AsyncBufReadExt;

mod proto {
    tonic::include_proto!("v1beta1");
}
mod uds;

#[derive(Debug, Deserialize)]
struct Config {
    hsm_types: Vec<HSMType>,
    p11_kit_binary: std::path::PathBuf,
}

#[derive(Debug, Deserialize)]
struct HSMType {
    name: String,
    tokens: Vec<String>,
    provider: Option<String>,
    max_clients: u32,
}

#[derive(Clone, Debug)]
struct AppState {
    name: String,
    tokens: Vec<String>,
    provider: Option<String>,
    p11_sock_path: std::path::PathBuf,
    ids: Vec<uuid::Uuid>
}

impl From<&HSMType> for AppState {
    fn from(config: &HSMType) -> Self {
        let runtime_dir = std::path::PathBuf::from(std::env::var("RUNTIME_DIRECTORY").expect("No runtime directory"));
        let mut ids = Vec::with_capacity(config.max_clients as usize);

        for _ in 0..config.max_clients {
            ids.push(uuid::Uuid::new_v4())
        }

        AppState {
            p11_sock_path: runtime_dir.join(std::path::PathBuf::from(format!("pkcs11-{}.sock", config.name))),
            name: config.name.clone(),
            tokens: config.tokens.clone(),
            provider: config.provider.clone(),
            ids,
        }
    }
}

struct DevicePlugin {
    state: std::sync::Arc<AppState>,
    should_exit: std::sync::Arc<std::sync::atomic::AtomicBool>
}

struct ListDevices {
    devices: Vec<proto::Device>,
    sent: bool,
    should_exit: std::sync::Arc<std::sync::atomic::AtomicBool>
}

#[tokio::main]
async fn main() {
    let args = clap::Command::new("k8s-p11-plugin")
        .author("Q Misell <q@as207960.net>")
        .version(env!("CARGO_PKG_VERSION"))
        .arg(clap::Arg::new("conf")
            .short('s')
            .long("settings")
            .takes_value(true)
            .default_value("./conf.yaml")
            .help("Where to read config file from"))
        .get_matches();

    if systemd_journal_logger::connected_to_journal() {
        systemd_journal_logger::init().unwrap();
        log::set_max_level(log::LevelFilter::Debug);
    } else {
        pretty_env_logger::init();
    }

    let conf_file = std::fs::File::open(args.value_of("conf").unwrap()).expect("Unable to open config file");
    let config = std::sync::Arc::new(serde_yaml::from_reader::<_, Config>(conf_file).unwrap());

    let mut self_socks = Vec::new();
    for hsm_type in &config.hsm_types {
        self_socks.push(std::path::PathBuf::from(format!("/var/lib/kubelet/device-plugins/as207960-hsm-plugin-{}.sock", hsm_type.name)));
    }

    let app_states = config.hsm_types.iter().map(|t| {
        std::sync::Arc::new(AppState::from(t))
    }).collect::<Vec<_>>();

    let (register_send, register_recv) = tokio::sync::mpsc::channel(1);
    let should_exit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (stops_send, mut stops_recv): (tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>, _) = tokio::sync::mpsc::unbounded_channel();
    let (request_stop_send, mut request_stop_recv) = tokio::sync::mpsc::unbounded_channel();
    let request_stop_send_ctrlc = request_stop_send.clone();
    let should_exit_ctrlc = should_exit.clone();
    let should_exit_register = should_exit.clone();
    ctrlc::set_handler(move || {
        info!("Exiting...");
        should_exit_ctrlc.store(true, std::sync::atomic::Ordering::Relaxed);
        request_stop_send_ctrlc.send(()).unwrap();
    }).expect("Error setting Ctrl-C handler");

    start_pkcs11_server(&config.p11_kit_binary, &app_states);

    let register_config = config.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        register_plugin(register_recv, register_config, should_exit_register).await;
    });

    let mut check_interval = tokio::time::interval(tokio::time::Duration::from_secs(2));

    let self_check_socks = self_socks.clone();
    tokio::spawn(async move {
        loop {
            check_interval.tick().await;
            if self_check_socks.iter().any(|s| !s.exists()) {
                info!("Socket deleted, recreating and registering...");
                request_stop_send.clone().send(()).unwrap();
            }
        }
    });
    tokio::spawn(async move {
        loop {
            request_stop_recv.recv().await;

            while let Some(stop_send) = stops_recv.recv().await {
                if let Err(_) = stop_send.send(()) {
                    info!("Already exiting...");
                }
            }
        }
    });

    'outer: loop {
        for (hsm_type, sock_path) in itertools::zip(app_states.iter(), self_socks.iter()) {
            let listener = uds::DeleteOnDrop::bind(sock_path).expect("Unable to create service socket");
            let _ = register_send.send(()).await;

            let svc = DevicePlugin {
                state: hsm_type.clone(),
                should_exit: should_exit.clone()
            };
            let (stop_send, stop_recv) = tokio::sync::oneshot::channel::<()>();
            stops_send.send(stop_send).unwrap();
            tonic::transport::Server::builder()
                .add_service(proto::device_plugin_server::DevicePluginServer::new(svc))
                .serve_with_incoming_shutdown(listener, stop_recv.map(|_| ()))
                .await
                .unwrap();
            if should_exit.load(std::sync::atomic::Ordering::Relaxed) {
                break 'outer;
            }
        }
    }
}

async fn register_plugin(
    mut msg_chan: tokio::sync::mpsc::Receiver<()>, config: std::sync::Arc<Config>,
    should_exit: std::sync::Arc<std::sync::atomic::AtomicBool>
) {
    while let Some(_) = msg_chan.recv().await {
        let sock = uds::UnixStream::connect("/var/lib/kubelet/device-plugins/kubelet.sock");
        let endpoint = tonic::transport::Endpoint::from_static("unix://null");
        let channel = match endpoint.connect_with_connector(sock).await {
            Ok(c) => c,
            Err(e) => {
                error!("Unable to connect to kubelet socket: {}", e);
                if should_exit.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                } else {
                    continue;
                }
            }
        };
        let mut client = proto::registration_client::RegistrationClient::new(channel);
        for hsm_type in &config.hsm_types {
            match client.register(proto::RegisterRequest {
                version: "v1beta1".to_string(),
                endpoint: format!("as207960-hsm-plugin-{}.sock", hsm_type.name),
                resource_name: format!("hsm.as207960.net/{}", hsm_type.name),
                options: None,
            }).await {
                Ok(_) => {
                    info!("Successfully registered with the kubelet")
                }
                Err(e) => {
                    error!("Unable to connect register with kubelet: {}", e);
                    break;
                }
            }
        }
    }
}

fn start_pkcs11_server(p11_kit_binary: &std::path::Path, configs: &[std::sync::Arc<AppState>]) {
    for hsm_type in configs {
        let hsm_type = hsm_type.clone();
        let p11_kit_binary = p11_kit_binary.to_path_buf();
        tokio::spawn(async move {
            let p11_kit_binary_path = p11_kit_binary.as_os_str();
            loop {
                let mut cmd = tokio::process::Command::new(p11_kit_binary_path);
                cmd.args(&["server", "-f", "-n"])
                    .arg(&hsm_type.p11_sock_path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                if let Some(provider) = &hsm_type.provider {
                    cmd.args(&["-p", provider]);
                }
                cmd.args(&hsm_type.tokens);

                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(err) => {
                        error!("Unable to start child PKCS#11 server: {}", err);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };
                info!("Started PKCS#11 server for token type {}", hsm_type.name);

                let mut stderr = tokio::io::BufReader::new(child.stderr.take().unwrap());
                tokio::spawn(async move {
                    loop {
                        let mut buf = Vec::new();
                        let read = match stderr.read_until('\n' as u8, &mut buf).await {
                            Ok(r) => r,
                            Err(err) => {
                                error!("Unable to read child process stderr: {}", err);
                                break;
                            }
                        };
                        if read == 0 {
                            break;
                        }
                        warn!("stderr from child process: {}", String::from_utf8_lossy(&buf));
                    }
                });

                let child_status = match child.wait().await {
                    Ok(s) => s,
                    Err(err) => {
                        error!("Unable to get child exit code: {}", err);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };
                if !child_status.success() {
                    error!("Non success child exit code: {}", child_status);
                } else {
                    warn!("Child exited, restarting");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }
}

#[tonic::async_trait]
impl proto::device_plugin_server::DevicePlugin for DevicePlugin {
    type ListAndWatchStream = std::pin::Pin<Box<dyn futures_util::stream::Stream<Item=Result<proto::ListAndWatchResponse, tonic::Status>> + Send + Sync>>;

    async fn get_device_plugin_options(
        &self,
        _request: tonic::Request<proto::Empty>,
    ) -> Result<tonic::Response<proto::DevicePluginOptions>, tonic::Status> {
        debug!("Got device plugin options request");
        Ok(tonic::Response::new(proto::DevicePluginOptions {
            pre_start_required: false,
            get_preferred_allocation_available: false,
        }))
    }

    async fn list_and_watch(
        &self,
        _request: tonic::Request<proto::Empty>,
    ) -> Result<tonic::Response<Self::ListAndWatchStream>, tonic::Status> {
        debug!("Got list and watch request");
        let devices = self.state.ids.iter().map(|id| {
            proto::Device {
                id: id.to_string(),
                health: "Healthy".to_string(),
                topology: None,
            }
        }).collect();
        Ok(tonic::Response::new(Box::pin(ListDevices {
            devices,
            sent: false,
            should_exit: self.should_exit.clone(),
        })))
    }

    async fn allocate(
        &self,
        request: tonic::Request<proto::AllocateRequest>,
    ) -> Result<tonic::Response<proto::AllocateResponse>, tonic::Status> {
        let msg = request.into_inner();
        debug!("Got allocate request: {:?}", msg);
        let resps = msg.container_requests.into_iter().map(|r| {
            for d in r.devices_ids {
                let id = match uuid::Uuid::parse_str(&d) {
                    Ok(i) => i,
                    Err(e) => return Err(tonic::Status::invalid_argument(e.to_string()))
                };
                if !self.state.ids.contains(&id) {
                    return Err(tonic::Status::not_found("Unknown device ID"));
                }
            }

            let container_path = format!("/dev/pkcs11-{}", self.state.name);
            Ok(proto::ContainerAllocateResponse {
                envs: std::collections::HashMap::from([
                    ("P11_KIT_SERVER_ADDRESS".to_string(), format!("unix:path={}", container_path))
                ]),
                mounts: vec![],
                annotations: std::collections::HashMap::new(),
                devices: vec![proto::DeviceSpec {
                    container_path,
                    host_path: self.state.p11_sock_path.to_string_lossy().to_string(),
                    permissions: "rw".to_string(),
                }],
            })
        }).collect::<Result<Vec<_>, tonic::Status>>()?;

        Ok(tonic::Response::new(proto::AllocateResponse {
            container_responses: resps
        }))
    }

    async fn get_preferred_allocation(
        &self,
        _request: tonic::Request<proto::PreferredAllocationRequest>,
    ) -> Result<tonic::Response<proto::PreferredAllocationResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(""))
    }

    async fn pre_start_container(
        &self,
        _request: tonic::Request<proto::PreStartContainerRequest>,
    ) -> Result<tonic::Response<proto::PreStartContainerResponse>, tonic::Status> {
        Err(tonic::Status::unimplemented(""))
    }
}


impl futures_util::stream::Stream for ListDevices {
    type Item = Result<proto::ListAndWatchResponse, tonic::Status>;

    fn poll_next(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if !this.sent {
            this.sent = true;
            debug!("Sending device list");
            std::task::Poll::Ready(Some(Ok(proto::ListAndWatchResponse {
                devices: this.devices.clone(),
            })))
        } else {
            if this.should_exit.load(std::sync::atomic::Ordering::Relaxed) {
                std::task::Poll::Ready(None)
            } else {
                std::task::Poll::Pending
            }
        }
    }
}
