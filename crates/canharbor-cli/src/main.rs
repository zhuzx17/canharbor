// SPDX-License-Identifier: AGPL-3.0-or-later
mod i18n;
mod sequence;
mod status;
mod stress;
mod stress_coordinate;
mod tasks;
use canharbor_core::runtime::Runtime;
use canharbor_server::{
    dto::{FrameDto, FrameKind, StreamMessage},
    local::{RunningServer, default_runtime_dir},
};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;
#[derive(Parser)]
#[command(
    name = "canharbor",
    version,
    about = "Local CAN/CAN FD workbench with explicit device selection.",
    after_help = "FIRST VIRTUAL RECEIVE/SEND:\n  canharbor serve --open\n  canharbor monitor --channel virtual:1 --listen-only\n  canharbor send --channel virtual:0 --id 123 --data 01020304 --active\nRun each command in a separate terminal. IDs are hexadecimal without 0x; bitrates are in bit/s.\nDiagnostics: canharbor doctor --devices\nA custom --runtime-dir must be shared by the service and all clients."
)]
struct Args {
    #[arg(long, global = true, value_parser = ["auto", "en", "zh-CN"], help = "Output language: auto, en or zh-CN; auto follows the saved preference or environment")]
    lang: Option<String>,
    #[arg(long, global = true)]
    #[arg(help = "Service runtime directory; use the same directory for all clients")]
    runtime_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Run the local service; defaults to two simulated channels
    Serve {
        #[arg(long)]
        #[arg(help = "Open a one-time paired browser after service startup")]
        open: bool,
        #[arg(long)]
        #[arg(help = "Run without the browser workbench (CLI-only)")]
        no_web: bool,
        #[arg(long, default_value_t = 0)]
        #[arg(help = "Loopback HTTP port; 0 chooses an available port")]
        port: u16,
        #[cfg(all(unix, feature = "kvaser"))]
        #[arg(long, requires = "kvaser_channel")]
        #[arg(help = "Absolute path to the compatible Kvaser native library")]
        kvaser_library: Option<PathBuf>,
        #[cfg(all(unix, feature = "kvaser"))]
        #[arg(long,requires="kvaser_library",value_parser=parse_kvaser_handle)]
        #[arg(help = "Explicit channel handle from kvaser-list")]
        kvaser_channel: Vec<u32>,
        #[cfg(all(unix, feature = "peak"))]
        #[arg(long, requires = "peak_channel")]
        #[arg(help = "Absolute path to the compatible PEAK native library")]
        peak_library: Option<PathBuf>,
        #[cfg(all(unix, feature = "peak"))]
        #[arg(long,requires="peak_library",value_parser=parse_peak_handle)]
        #[arg(help = "Explicit hexadecimal channel handle from peak-list")]
        peak_channel: Vec<u16>,
        #[cfg(feature = "slcan")]
        #[arg(long, requires = "serial_baud")]
        #[arg(help = "Absolute serial device path, for example /dev/cu.usbmodem123")]
        slcan: Option<PathBuf>,
        #[cfg(feature = "slcan")]
        #[arg(long, requires = "slcan")]
        #[arg(help = "Serial link baud rate, separate from CAN bitrate")]
        serial_baud: Option<u32>,
        #[cfg(feature = "slcan")]
        #[arg(long, requires = "slcan")]
        slcan_polling: bool,
        /// Explicit firmware dialect; CANable FD commands have no firmware acknowledgement.
        #[cfg(feature = "slcan")]
        #[arg(long, requires = "slcan", value_parser = ["lawicel", "canable-fd"])]
        slcan_profile: Option<String>,
        #[arg(long,requires_all=["usb_index","usb_interface","usb_alternate"])]
        #[arg(
            help = "Explicit gs_usb vendor:product IDs in hexadecimal; enumerate with usb-list first"
        )]
        usb: Option<String>,
        #[arg(long, requires = "usb")]
        #[arg(
            help = "Index from the latest usb-list result; verify identity after every reconnect"
        )]
        usb_index: Option<usize>,
        #[arg(long, requires = "usb")]
        #[arg(help = "USB interface number from usb-list")]
        usb_interface: Option<u8>,
        #[arg(long, requires = "usb")]
        #[arg(help = "USB alternate setting from usb-list")]
        usb_alternate: Option<u8>,
    },
    /// Create and inspect bounded periodic transmission tasks
    Task {
        #[command(subcommand)]
        command: tasks::Command,
    },
    /// Validate, run and inspect stress tests
    Stress {
        #[command(subcommand)]
        command: stress::Command,
    },
    /// Send or receive a bounded communication-check sequence
    Sequence {
        #[command(subcommand)]
        command: sequence::Command,
    },
    /// List channels registered by the running service
    Devices {
        #[arg(long)]
        #[arg(help = "Print machine-readable JSON")]
        json: bool,
    },
    /// Record, download and inspect traffic
    Capture {
        #[command(subcommand)]
        command: CaptureCommand,
    },
    /// Read service diagnostics without opening or starting channels
    Doctor {
        /// Include a read-only device report; writes JSON to stdout.
        #[arg(long)]
        devices: bool,
    },
    /// Read a previous transmission result without resending
    Receipt {
        #[arg(long)]
        #[arg(help = "Existing opaque session ID from canharbor session list")]
        session: String,
        #[arg(long, required_unless_present = "request", conflicts_with = "request")]
        receipt: Option<Uuid>,
        #[arg(long, required_unless_present = "receipt", conflicts_with = "receipt")]
        request: Option<String>,
        #[arg(long, requires = "request")]
        #[arg(help = "Original lease generation for unambiguous read-only result lookup")]
        generation: Option<String>,
    },
    /// Read the advertised capabilities of a channel
    Capabilities {
        #[arg(long)]
        #[arg(help = "Channel ID from canharbor devices, for example virtual:0")]
        channel: String,
    },
    #[cfg(all(unix, feature = "kvaser"))]
    /// Enumerate Kvaser channels using an explicitly selected native library
    KvaserList {
        #[arg(long)]
        library: PathBuf,
    },
    #[cfg(all(unix, feature = "peak"))]
    /// Enumerate PEAK channels using an explicitly selected native library
    PeakList {
        #[arg(long)]
        library: PathBuf,
    },
    /// Enumerate matching USB devices without starting CAN acquisition
    UsbList {
        #[arg(long)]
        #[arg(help = "USB vendor:product IDs in hexadecimal, for example 1d50:606f")]
        vid_pid: String,
    },
    /// Open or print a fresh browser pairing link
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
    /// List or open channel sessions
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Observe traffic; --channel creates a foreground listener until interrupted
    Monitor {
        #[arg(long, conflicts_with = "channel")]
        #[arg(help = "Existing opaque session ID from canharbor session list")]
        session: Option<String>,
        #[arg(long, conflicts_with = "session")]
        #[arg(help = "Channel ID from canharbor devices, for example virtual:0")]
        channel: Option<String>,
        #[arg(long, default_value_t = 500000)]
        #[arg(help = "Nominal CAN bitrate in bit/s, for example 500000")]
        bitrate: u32,
        #[arg(long)]
        #[arg(help = "Enable CAN FD in this session")]
        fd: bool,
        #[arg(long, requires = "fd")]
        #[arg(help = "CAN FD data bitrate in bit/s; requires --fd (default 2000000)")]
        data_bitrate: Option<u32>,
        #[arg(long)]
        #[arg(help = "Observe without participating in CAN ACK or transmitting")]
        listen_only: bool,
        #[arg(long)]
        #[arg(help = "Maximum number of frames; see the command description for bounds")]
        count: Option<usize>,
    },
    /// Submit one frame with explicit --active; does not retry an unknown result
    #[command(
        after_help = "Example: canharbor send --channel virtual:0 --id 123 --data 01020304 --active\nCAN FD: add --fd --data-bitrate 2000000 --brs; payload length must be an exact CAN FD wire length.\nUnknown result (exit 7): inspect the printed request/session identifiers with receipt; do not resend blindly."
    )]
    Send {
        #[arg(long)]
        #[arg(help = "Channel ID from canharbor devices, for example virtual:0")]
        channel: String,
        #[arg(long)]
        #[arg(help = "CAN identifier in hexadecimal without 0x, for example 123")]
        id: String,
        #[arg(long, default_value = "")]
        #[arg(help = "Payload as complete hexadecimal bytes without spaces, for example 01020304")]
        data: String,
        #[arg(long)]
        #[arg(help = "Use a 29-bit identifier instead of the default 11-bit identifier")]
        extended: bool,
        #[arg(long)]
        #[arg(help = "Enable CAN FD in this session")]
        fd: bool,
        #[arg(long, requires = "fd")]
        #[arg(help = "CAN FD data bitrate in bit/s; requires --fd (default 2000000)")]
        data_bitrate: Option<u32>,
        #[arg(long)]
        #[arg(help = "Use CAN FD bit-rate switching; requires --fd")]
        brs: bool,
        #[arg(long)]
        #[arg(help = "Explicitly authorize active CAN operation; sending requires this flag")]
        active: bool,
        #[arg(long, default_value_t = 500000)]
        #[arg(help = "Nominal CAN bitrate in bit/s, for example 500000")]
        bitrate: u32,
    },
}
#[derive(Subcommand)]
enum CaptureCommand {
    /// Inspect a bounded local capture without contacting the service.
    Inspect {
        input: PathBuf,
        #[arg(long, default_value_t=100, value_parser=clap::value_parser!(u16).range(0..=500))]
        limit: u16,
    },
    /// Start a bounded recording of an already running session
    Start {
        #[arg(long)]
        #[arg(help = "Existing opaque session ID from canharbor session list")]
        session: String,
        #[arg(long)]
        #[arg(help = "Unique ID for this logical request; retain it for read-only recovery")]
        request_id: Option<String>,
        #[arg(long, default_value_t=3600, value_parser=clap::value_parser!(u32).range(1..=86400))]
        duration: u32,
        /// Optional printable recording name (up to 120 characters).
        #[arg(long, default_value = "")]
        name: String,
    },
    /// List retained recordings and their completion state
    List,
    /// Read one recording status
    Status { id: Uuid },
    /// Finish a recording without stopping the channel
    Stop { id: Uuid },
    /// Download a stopped recording (up to 64 MiB)
    Download {
        id: Uuid,
        #[arg(long)]
        #[arg(help = "Destination file path")]
        output: PathBuf,
    },
    /// Delete a stopped recording and its stored file
    Delete { id: Uuid },
}
#[derive(Subcommand)]
enum WebCommand {
    /// Pair a browser; opening the link never starts a channel
    Open {
        #[arg(long)]
        #[arg(
            help = "Print the one-time pairing link instead of opening a browser (expires in 60 seconds)"
        )]
        print: bool,
    },
}
#[derive(Subcommand)]
enum SessionCommand {
    /// List sessions, applied settings and ownership
    List,
    /// Open a configured session without starting acquisition
    Open {
        #[arg(long)]
        #[arg(help = "Channel ID from canharbor devices, for example virtual:0")]
        channel: String,
        #[arg(long, default_value_t = 500000)]
        #[arg(help = "Nominal CAN bitrate in bit/s, for example 500000")]
        bitrate: u32,
        #[arg(long)]
        #[arg(help = "Enable CAN FD in this session")]
        fd: bool,
        #[arg(long, requires = "fd")]
        #[arg(help = "CAN FD data bitrate in bit/s; requires --fd (default 2000000)")]
        data_bitrate: Option<u32>,
        #[arg(long)]
        #[arg(help = "Observe without participating in CAN ACK or transmitting")]
        listen_only: bool,
    },
}
struct Client {
    local: canharbor_client::LocalClient,
}
impl std::ops::Deref for Client {
    type Target = canharbor_client::LocalClient;
    fn deref(&self) -> &Self::Target {
        &self.local
    }
}
impl Client {
    fn new(dir: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            local: canharbor_client::LocalClient::new(dir.clone())?,
        })
    }
    async fn open_with_data(
        &self,
        channel: &str,
        bitrate: u32,
        active: bool,
        data_bitrate: Option<u32>,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        self.request("POST","/sessions",json!({"channel_id":channel,"config":{"mode":if active{"active"}else{"listen_only"},"nominal_bitrate":bitrate,"fd":data_bitrate.map(|rate|json!({"enabled":true,"data_bitrate":rate,"iso":true}))}}),None).await
    }

    async fn control(&self, id: &str) -> Result<String, Box<dyn std::error::Error>> {
        let v = self
            .request("POST", &format!("/sessions/{id}/lease"), json!({}), None)
            .await?;
        Ok(v["generation"]
            .as_str()
            .ok_or("invalid lease response")?
            .into())
    }
    async fn stream(
        &self,
        session: &str,
        count: Option<usize>,
        lease: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut ws = self
            .local
            .subscribe(
                session,
                &canharbor_client::api::Subscribe {
                    r#type: canharbor_client::api::SubscribeKind::Subscribe,
                    protocol_version: "1.0".into(),
                    csrf_token: None,
                    after_sequence: None,
                    filter: canharbor_client::api::Filter::default(),
                },
            )
            .await?;
        let mut renew = tokio::time::interval(Duration::from_secs(5));
        let mut seen = 0;
        loop {
            tokio::select! {
            _=tokio::signal::ctrl_c()=>break,
            _=renew.tick(),if lease.is_some()=>{self.request("POST",&format!("/sessions/{session}/lease/renew"),json!({}),lease).await?;},
            next=ws.next()=>{match next? {Some(message)=>{
                match message {
                    StreamMessage::Events{events,..} => {
                        for event in events {
                            println!("{}",serde_json::to_string(&event)?);
                            if matches!(event,canharbor_server::dto::BusEvent::FrameReceived{..}) {seen+=1;}
                            if count.is_some_and(|c|seen>=c) {ws.close().await?;return Ok(())}
                        }
                    }
                    message @ StreamMessage::Gap{..} => eprintln!("{}: {}",i18n::choose("stream gap", "事件流丢失"),serde_json::to_string(&message)?),
                    message @ StreamMessage::SessionFault{..} => {
                        let StreamMessage::SessionFault{code,..}=&message else {unreachable!()};
                        return Err(Box::new(status::Failure::Reported{code:if matches!(code,canharbor_server::dto::FaultCode::DeviceDisconnected){4}else{6},message:format!("channel fault: {}",serde_json::to_string(&message)?)}));
                    },
                    StreamMessage::ResumeUnavailable => return Err("stream history is unavailable".into()),
                    // Hello confirms the server has installed the subscription.
                    StreamMessage::Hello{..} => {
                        if let Some(g) = lease {
                            self.request("POST", &format!("/sessions/{session}/start"), json!({}), Some(g)).await?;
                        }
                    },
                }},None=>return Err(Box::new(status::Failure::Reported {code:3,message:"event stream closed; no automatic resume".into()}))}}
            }
        }
        ws.close().await?;
        Ok(())
    }
}
fn print(value: Value) {
    println!("{}", serde_json::to_string_pretty(&value).unwrap())
}
#[cfg(unix)]
fn browser(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(url).status()?;
    #[cfg(not(target_os = "macos"))]
    let status = std::process::Command::new("xdg-open").arg(url).status()?;
    if !status.success() {
        return Err("could not open browser; use `canharbor web open --print`".into());
    }
    Ok(())
}
#[cfg(windows)]
fn browser(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    let operation: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize <= 32 {
        return Err("could not open browser; use `canharbor web open --print`".into());
    }
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        i18n::report_error(e.as_ref());
        std::process::exit(status::exit_code(e.as_ref()))
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    i18n::initialize();
    let matches = i18n::command(Args::command())
        .try_get_matches()
        .unwrap_or_else(|error| {
            if i18n::chinese()
                && !matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                )
            {
                eprintln!("{}\n技术详情：", i18n::error_summary(2));
            }
            error.exit()
        });
    let args = Args::from_arg_matches(&matches)?;
    let dir = args.runtime_dir.unwrap_or_else(default_runtime_dir);
    if let Command::Capture {
        command: CaptureCommand::Inspect { input, limit },
    } = &args.command
    {
        let file = canharbor_platform::files::open_regular(input).map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidInput {
                status::input(error.to_string())
            } else {
                Box::new(error) as Box<dyn std::error::Error>
            }
        })?;
        let result = canharbor_replay::inspect(file, usize::from(*limit))
            .map_err(|e| status::input(e.to_string()))?;
        print(serde_json::to_value(result)?);
        return Ok(());
    }
    if let Command::Serve {
        open,
        no_web,
        port,
        usb,
        usb_index,
        usb_interface,
        usb_alternate,
        #[cfg(all(unix, feature = "kvaser"))]
        kvaser_library,
        #[cfg(all(unix, feature = "kvaser"))]
        kvaser_channel,
        #[cfg(all(unix, feature = "peak"))]
        peak_library,
        #[cfg(all(unix, feature = "peak"))]
        peak_channel,
        #[cfg(feature = "slcan")]
        slcan,
        #[cfg(feature = "slcan")]
        serial_baud,
        #[cfg(feature = "slcan")]
        slcan_polling,
        #[cfg(feature = "slcan")]
        slcan_profile,
    } = args.command
    {
        #[cfg(windows)]
        let mut shutdown = {
            // A launcher may pass an inherited "ignore Ctrl-C" attribute.
            // Register our handler before restoring normal delivery; never leave
            // a readiness window in which the default handler bypasses cleanup.
            let signal = tokio::signal::windows::ctrl_c()?;
            if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            signal
        };
        if open && no_web {
            return Err(status::input("--open conflicts with --no-web"));
        }
        let mut backends: Vec<Arc<dyn canharbor_core::Backend>> =
            vec![Arc::new(canharbor_virtual::VirtualBus::default())];
        if let Some(vid_pid) = usb {
            let id = parse_vid_pid(&vid_pid)?;
            let transport = tokio::task::spawn_blocking(move || {
                let candidates = canharbor_gsusb::usb::discover(&[id])?;
                let candidate = candidates
                    .into_iter()
                    .nth(usb_index.unwrap())
                    .ok_or(canharbor_gsusb::Error::Protocol("USB candidate index"))?;
                candidate.open(usb_interface.unwrap(), usb_alternate.unwrap())
            })
            .await??;
            backends.push(Arc::new(
                canharbor_gsusb::backend::GsUsbBackend::open(
                    transport,
                    u16::from(usb_interface.unwrap()),
                    Uuid::new_v4().simple().to_string(),
                )
                .await?,
            ));
        }
        #[cfg(feature = "slcan")]
        if let Some(path) = slcan {
            let profile = match slcan_profile.as_deref().unwrap_or("lawicel") {
                "lawicel" => canharbor_slcan::Profile::Lawicel,
                "canable-fd" => canharbor_slcan::Profile::CanableFd,
                _ => unreachable!("validated by clap"),
            };
            if slcan_polling && profile == canharbor_slcan::Profile::CanableFd {
                return Err(status::input(
                    "--slcan-polling is unsupported with --slcan-profile canable-fd",
                ));
            }
            let baud =
                serial_baud.ok_or_else(|| status::input("--slcan requires --serial-baud"))?;
            let transport = tokio::task::spawn_blocking(move || {
                canharbor_slcan::serial::Serial::open(&path, baud)
            })
            .await??;
            backends.push(Arc::new(
                canharbor_slcan::backend::SlcanBackend::open_with_profile(
                    transport,
                    Uuid::new_v4().simple().to_string(),
                    slcan_polling,
                    profile,
                )
                .await?,
            ));
        }
        #[cfg(all(unix, feature = "kvaser"))]
        if let Some(path) = kvaser_library {
            // SAFETY: explicit startup path selects trusted native code with the
            // documented open-source libKvaserUSB ABI; no HTTP path can load code.
            let backend = canharbor_driver::DriverBackend::open(
                move || unsafe {
                    canharbor_kvaser::KvaserDriver::load(&path, &kvaser_channel)
                        .map(|driver| Box::new(driver) as Box<dyn canharbor_driver::Driver>)
                },
                Uuid::new_v4().simple().to_string(),
            )
            .await?;
            backends.push(Arc::new(backend));
        }
        #[cfg(all(unix, feature = "peak"))]
        if let Some(path) = peak_library {
            // SAFETY: this explicit startup option selects trusted native code
            // implementing the documented libPeakUSB 1.0.2 ABI. No HTTP input
            // can load a library or choose a filesystem path.
            let backend = canharbor_driver::DriverBackend::open(
                move || unsafe {
                    canharbor_peak::PeakDriver::load(&path, &peak_channel)
                        .map(|driver| Box::new(driver) as Box<dyn canharbor_driver::Driver>)
                },
                Uuid::new_v4().simple().to_string(),
            )
            .await?;
            backends.push(Arc::new(backend));
        }
        let server = RunningServer::start(
            Runtime::new(Arc::new(canharbor_core::registry::Registry::new(backends)?)),
            &dir,
            port,
            !no_web,
        )
        .await?;
        eprintln!(
            "{}{}",
            i18n::choose(
                "CANHarbor: local service ready",
                "CANHarbor：本地服务已就绪"
            ),
            if no_web {
                String::new()
            } else {
                format!("{}{}", i18n::choose(" at ", "，地址 "), server.state.origin)
            }
        );
        if open {
            let client = Client::new(dir.clone())?;
            let v = client
                .request("POST", "/native/pair", json!({}), None)
                .await?;
            if let Err(e) = browser(v["url"].as_str().ok_or("invalid pairing response")?) {
                eprintln!("{e}")
            }
        }
        #[cfg(unix)]
        tokio::signal::ctrl_c().await?;
        #[cfg(windows)]
        shutdown
            .recv()
            .await
            .ok_or("console signal stream closed")?;
        let report = server.shutdown().await;
        let failed = !report.failures.is_empty();
        for failure in report.failures {
            eprintln!("{}: {failure}", i18n::choose("shutdown", "关闭"));
        }
        if report.unknown_transmissions > 0 || report.unknown_tasks > 0 {
            return Err(status::unknown(format!(
                "Unconfirmed transmissions at shutdown: {}; tasks with unknown outcomes: {}",
                report.unknown_transmissions, report.unknown_tasks
            )));
        }
        if failed {
            return Err("service shutdown incomplete".into());
        }
        return Ok(());
    }
    let client = Client::new(dir)?;
    match args.command {
        Command::Task { command } => tasks::run(&client.local, command).await?,
        Command::Stress { command } => stress::run(&client.local, command).await?,
        Command::Sequence { command } => sequence::run(&client.local, command).await?,
        Command::Capture {command} => match command {
            CaptureCommand::Inspect {..} => unreachable!(),
            CaptureCommand::Start {session,request_id,duration,name} => print(client.request("POST","/captures",json!({"session_id":session,"client_request_id":request_id.unwrap_or_else(||Uuid::new_v4().to_string()),"max_duration_seconds":duration,"name":name}),None).await?),
            CaptureCommand::List => print(client.request("GET","/captures",Value::Null,None).await?),
            CaptureCommand::Status {id} => print(client.request("GET",&format!("/captures/{id}"),Value::Null,None).await?),
            CaptureCommand::Stop {id} => print(client.request("POST",&format!("/captures/{id}/stop"),json!({}),None).await?),
            CaptureCommand::Delete {id} => {client.request("DELETE",&format!("/captures/{id}"),json!({}),None).await?;},
            CaptureCommand::Download {id,output} => client.download(id,output).await?,
        },
        Command::Devices { json: as_json } => {
            let value = client.request("GET", "/devices", Value::Null, None).await?;
            if as_json {
                print(value)
            } else {
                for device in value["devices"].as_array().ok_or("invalid devices")? {
                    println!(
                        "{} ({})",
                        device["label"].as_str().unwrap_or("CAN device"),
                        if device["simulated"].as_bool() == Some(true) {
                            i18n::choose("simulated", "模拟")
                        } else {
                            i18n::choose("hardware, unverified", "硬件，未验证")
                        }
                    );
                    for c in device["channels"].as_array().ok_or("invalid channels")? {
                        println!("  {}", c["id"].as_str().unwrap_or("unknown"));
                    }
                }
            }
        }
        Command::Capabilities { channel } => print(
            client
                .request(
                    "GET",
                    &format!("/channels/{channel}/capabilities"),
                    Value::Null,
                    None,
                )
                .await?,
        ),
        Command::Receipt{session,receipt,request,generation}=>{
            let result=if let Some(receipt)=receipt{client.receipt(&session,receipt).await?}else{client.receipt_by_request(&session,request.as_deref().unwrap(),generation.as_deref()).await?};
            print(serde_json::to_value(result)?);
        },
        Command::Doctor { devices } => print(
            client
                .request("GET", if devices { "/diagnostics/devices" } else { "/diagnostics" }, Value::Null, None)
                .await?,
        ),
        Command::Web {
            command: WebCommand::Open { print: as_text },
        } => {
            let v = client
                .request("POST", "/native/pair", json!({}), None)
                .await?;
            let url = v["url"].as_str().ok_or("invalid pair URL")?;
            if as_text {
                println!("{url}")
            } else {
                browser(url)?
            }
        }
        Command::Session {
            command: SessionCommand::List,
        } => print(
            client
                .request("GET", "/sessions", Value::Null, None)
                .await?,
        ),
        Command::Session {
            command:
                SessionCommand::Open {
                    channel,
                    bitrate,
                    fd,
                    data_bitrate,
                    listen_only,
                },
        } => {
            if !listen_only {
                return Err(status::input("session open requires --listen-only; use send --active for one-shot transmission"));
            }
            print(
                client
                    .open_with_data(
                        &channel,
                        bitrate,
                        false,
                        fd.then_some(data_bitrate.unwrap_or(2_000_000)),
                    )
                    .await?,
            )
        }
        Command::Monitor {
            session,
            channel,
            bitrate,
            fd,
            data_bitrate,
            listen_only,
            count,
        } => {
            if count == Some(0) {
                return Err(status::input("--count must be positive"));
            }
            if let Some(id) = session {
                client.stream(&id, count, None).await?
            } else {
                if !listen_only {
                    return Err(status::input("channel monitor requires --listen-only"));
                }
                let channel = channel.ok_or_else(||status::input("provide --session or --channel"))?;
                let s = client
                    .open_with_data(
                        &channel,
                        bitrate,
                        false,
                        fd.then_some(data_bitrate.unwrap_or(2_000_000)),
                    )
                    .await?;
                let id = s["id"].as_str().ok_or("invalid session")?;
                let g = client.control(id).await?;
                let result = client.stream(id, count, Some(&g)).await;
                let _ = client
                    .request("DELETE", &format!("/sessions/{id}"), json!({}), Some(&g))
                    .await;
                result?
            }
        }
        Command::Send {
            channel,
            id,
            data,
            extended,
            fd,
            data_bitrate,
            brs,
            active,
            bitrate,
        } => {
            if !active {
                return Err(status::input("sending requires explicit --active"));
            }
            let id = u32::from_str_radix(&id, 16)
                .map_err(|_|status::input("--id must be hexadecimal without a prefix"))?;
            let dto = FrameDto {
                kind: if fd {
                    FrameKind::FdData
                } else {
                    FrameKind::ClassicData
                },
                id,
                extended,
                brs,
                data_hex: data,
                dlc: None,
            };
            dto.clone()
                .into_frame()
                .map_err(|e|status::input(format!("invalid frame: {e}")))?;
            let s = client
                .open_with_data(
                    &channel,
                    bitrate,
                    true,
                    fd.then_some(data_bitrate.unwrap_or(2_000_000)),
                )
                .await?;
            let sid = s["id"].as_str().ok_or("invalid session")?;
            let g = client.control(sid).await?;
            let result = async {
                client
                    .request(
                        "POST",
                        &format!("/sessions/{sid}/start"),
                        json!({}),
                        Some(&g),
                    )
                    .await?;
                let request_id=Uuid::new_v4().to_string();
                let endpoint=format!("/sessions/{sid}/tx");
                let request=client.request("POST",&endpoint,json!({"client_request_id":request_id,"frame":dto}),Some(&g));
                tokio::select!{
                    result=request=>match result {
                        Ok(value)=>Ok(value),
                        Err(error)=>Err(status::submission_error(error,sid,&request_id,&g)),
                    },
                    signal=tokio::signal::ctrl_c()=>{
                        let reason=if signal.is_ok(){"interrupted while awaiting submission"}else{"signal monitoring failed while awaiting submission"};
                        Err(status::unknown(format!("{reason}; outcome unknown; session {sid}, request {request_id}; inspect with canharbor receipt --session {sid} --request {request_id} --generation {g}; do not resend automatically")))
                    },
                }
            }
            .await;
            let cleanup_endpoint=format!("/sessions/{sid}");
            let _=tokio::time::timeout(Duration::from_secs(2),client.request("DELETE",&cleanup_endpoint,json!({}),Some(&g))).await;
            print(result?)
        }
        #[cfg(all(unix, feature = "kvaser"))]
        Command::KvaserList { library } => {
            // SAFETY: explicit native-code selection under the documented ABI contract.
            let devices=tokio::task::spawn_blocking(move||unsafe {canharbor_kvaser::discover(&library)}).await??;
            print(json!({"backend":"kvaser","verification":"hardware_unverified","library_abi":"1.0.2","channels":devices}));
        },
        #[cfg(all(unix, feature = "peak"))]
        Command::PeakList {library} => {
            // SAFETY: the user explicitly selects a trusted implementation of
            // this native ABI. Discovery does not initialize a controller.
            let devices=tokio::task::spawn_blocking(move||unsafe {canharbor_peak::discover(&library)}).await??;
            print(json!({"backend":"peak","verification":"hardware_unverified","library_abi":"1.0.2","channels":devices.into_iter().map(|device|json!({"handle":format!("0x{:X}",device.handle),"name":device.name,"fd":device.fd,"available":device.available,"device_id":device.device_id})).collect::<Vec<_>>() }));
        }
        Command::UsbList { vid_pid } => {
            let id = parse_vid_pid(&vid_pid)?;
            let candidates =
                tokio::task::spawn_blocking(move || canharbor_gsusb::usb::discover(&[id]))
                    .await??;
            print(
                json!({"candidates":candidates.iter().enumerate().map(|(index,c)|json!({"index":index,"vendor_id":c.id().vendor,"product_id":c.id().product,"has_serial":c.serial().is_some()})).collect::<Vec<_>>()}),
            );
        }
        Command::Serve { .. } => unreachable!(),
    };
    Ok(())
}

#[cfg(all(unix, feature = "peak"))]
fn parse_peak_handle(value: &str) -> Result<u16, String> {
    let digits = value
        .strip_prefix("0x")
        .ok_or("PEAK channel must use an explicit 0x hexadecimal prefix")?;
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("invalid PEAK channel handle".into());
    }
    let handle = u16::from_str_radix(digits, 16).map_err(|_| "invalid PEAK channel handle")?;
    if !(0x51..=0x58).contains(&handle) && !(0x509..=0x510).contains(&handle) {
        return Err("unsupported PEAK USB channel handle".into());
    }
    Ok(handle)
}

fn parse_vid_pid(value: &str) -> Result<canharbor_gsusb::usb::VidPid, Box<dyn std::error::Error>> {
    let (vendor, product) = value
        .split_once(':')
        .ok_or_else(|| status::input("VID:PID must contain four hex digits per component"))?;
    if vendor.len() != 4
        || product.len() != 4
        || !vendor
            .bytes()
            .chain(product.bytes())
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(status::input(
            "VID:PID must contain four hex digits per component",
        ));
    }
    Ok(canharbor_gsusb::usb::VidPid {
        vendor: u16::from_str_radix(vendor, 16)?,
        product: u16::from_str_radix(product, 16)?,
    })
}

#[cfg(all(unix, feature = "kvaser"))]
fn parse_kvaser_handle(value: &str) -> Result<u32, String> {
    let number = value
        .strip_prefix("0x")
        .ok_or("expected a hexadecimal handle prefixed with 0x")?;
    let handle = u32::from_str_radix(number, 16).map_err(|_| "invalid Kvaser handle")?;
    if handle == 0 {
        return Err("handle zero is not a channel".into());
    }
    Ok(handle)
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn usb_open_requires_all_explicit_selection_fields() {
        assert!(Args::try_parse_from(["canharbor", "serve", "--usb", "1d50:606f"]).is_err());
        assert!(Args::try_parse_from(["canharbor", "serve", "--usb-index", "0"]).is_err());
        assert!(
            Args::try_parse_from([
                "canharbor",
                "serve",
                "--usb",
                "1d50:606f",
                "--usb-index",
                "0",
                "--usb-interface",
                "0",
                "--usb-alternate",
                "0"
            ])
            .is_ok()
        );
    }
    #[cfg(feature = "slcan")]
    #[test]
    fn serial_open_requires_explicit_path_and_baud() {
        assert!(Args::try_parse_from(["canharbor", "serve", "--slcan", "/dev/null"]).is_err());
        assert!(Args::try_parse_from(["canharbor", "serve", "--serial-baud", "115200"]).is_err());
        assert!(Args::try_parse_from(["canharbor", "serve", "--slcan-polling"]).is_err());
        assert!(
            Args::try_parse_from(["canharbor", "serve", "--slcan-profile", "canable-fd"]).is_err()
        );
        assert!(
            Args::try_parse_from([
                "canharbor",
                "serve",
                "--slcan",
                "/dev/null",
                "--serial-baud",
                "115200",
                "--slcan-profile",
                "unknown"
            ])
            .is_err()
        );
        assert!(
            Args::try_parse_from([
                "canharbor",
                "serve",
                "--slcan",
                "/dev/null",
                "--serial-baud",
                "115200"
            ])
            .is_ok()
        );
    }
    #[cfg(all(unix, feature = "peak"))]
    #[test]
    fn peak_requires_explicit_library_and_handle() {
        assert!(
            Args::try_parse_from([
                "canharbor",
                "serve",
                "--peak-library",
                "/tmp/libPeakUSB.dylib"
            ])
            .is_err()
        );
        assert!(Args::try_parse_from(["canharbor", "serve", "--peak-channel", "0x51"]).is_err());
        assert!(
            Args::try_parse_from([
                "canharbor",
                "serve",
                "--peak-library",
                "/tmp/libPeakUSB.dylib",
                "--peak-channel",
                "0x51",
                "--peak-channel",
                "0x52"
            ])
            .is_ok()
        );
        for invalid in ["51", "0x0", "0x59", "0xFFFF", "0xGG"] {
            assert!(parse_peak_handle(invalid).is_err());
        }
    }
    #[test]
    fn usb_ids_and_fd_arguments_are_unambiguous() {
        assert!(parse_vid_pid("1d50:606f").is_ok());
        for bad in ["1:2", "0x1d50:606f", "1d50:606f:0", "ZZZZ:0000"] {
            assert!(parse_vid_pid(bad).is_err());
        }
        assert!(
            Args::try_parse_from([
                "canharbor",
                "send",
                "--channel",
                "virtual:0",
                "--id",
                "123",
                "--data-bitrate",
                "2000000"
            ])
            .is_err()
        );
    }
    #[cfg(all(unix, feature = "kvaser"))]
    #[test]
    fn kvaser_requires_explicit_library_and_nonzero_handle() {
        assert!(
            Args::try_parse_from(["canharbor", "serve", "--kvaser-library", "/tmp/lib.so"])
                .is_err()
        );
        assert!(
            Args::try_parse_from(["canharbor", "serve", "--kvaser-channel", "0x10200"]).is_err()
        );
        assert!(
            Args::try_parse_from([
                "canharbor",
                "serve",
                "--kvaser-library",
                "/tmp/lib.so",
                "--kvaser-channel",
                "0x10200",
                "--kvaser-channel",
                "0x10201"
            ])
            .is_ok()
        );
        for value in ["0x0", "0", "-1", "0xFFFFFFFFF"] {
            assert!(parse_kvaser_handle(value).is_err());
        }
    }
}
