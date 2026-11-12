// SPDX-License-Identifier: AGPL-3.0-or-later
//! Optional adapter for the open-source libKvaserUSB 1.0.2 API.
mod ffi;
use canharbor_core::{
    BackendFault, CanId, Capabilities, CloseSummary, Config, Diagnostic, Error, Frame,
    FrameMetadata,
    timestamp::{Quality, Stamp},
};
use canharbor_driver::{Descriptor, Driver, Event, library::Claim};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::CStr,
    path::Path,
};
const NOMINAL: [u32; 9] = [
    10_000, 20_000, 50_000, 100_000, 125_000, 250_000, 500_000, 800_000, 1_000_000,
];
const DATA: [u32; 4] = [1_000_000, 2_000_000, 4_000_000, 8_000_000];
const LENGTHS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64];
#[derive(Clone, Debug, serde::Serialize)]
pub struct Device {
    pub handle: u32,
    pub name: String,
    pub product: String,
    pub pid: u16,
    pub channel: u8,
    pub serial: u32,
    pub fd: bool,
    pub available: bool,
}
fn error(code: i32) -> Error {
    match code {
        -2 | -5 => Error::InvalidState,
        -7 => Error::Fault(BackendFault::BusOff),
        -8 | -9 => Error::Unsupported,
        -10 => Error::QueueFull,
        _ => Error::Fault(BackendFault::Disconnected),
    }
}
fn checked(code: i32) -> Result<(), Error> {
    if code == 0 { Ok(()) } else { Err(error(code)) }
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.split(|v| *v == 0).next().unwrap_or_default()).into_owned()
}
fn inspect(api: &ffi::Api) -> Result<Vec<Device>, Error> {
    // SAFETY: API contract guarantees a live NUL-terminated version string.
    let version = unsafe { (api.version)() };
    if version.is_null() || unsafe { CStr::from_ptr(version) }.to_bytes() != b"1.0.2" {
        return Err(Error::Unsupported);
    }
    let mut channels = [ffi::Channel::default(); 32];
    // SAFETY: array capacity and repr(C) match the supplied count and retained header.
    let count = unsafe { (api.scan)(channels.as_mut_ptr(), 32) };
    if count < 0 {
        return Err(error(count));
    }
    if count > 32 {
        return Err(Error::Unsupported);
    }
    let mut seen = BTreeSet::new();
    let mut devices = vec![];
    for c in &channels[..count as usize] {
        if c.handle == 0
            || !seen.insert(c.handle)
            || c.fd > 1
            || c.available > 1
            || c.channel >= c.channel_count
        {
            return Err(Error::Unsupported);
        }
        devices.push(Device {
            handle: c.handle,
            name: text(&c.name),
            product: text(&c.product),
            pid: c.pid,
            channel: c.channel,
            serial: c.serial,
            fd: c.fd != 0,
            available: c.available != 0,
        });
    }
    Ok(devices)
}
/// Enumerate channels without opening a controller.
/// # Safety
/// The absolute path must select a trusted libKvaserUSB 1.0.2 image implementing
/// the retained ABI. Native constructors and all pointer contracts are trusted.
pub unsafe fn discover(path: &Path) -> Result<Vec<Device>, Error> {
    // SAFETY: caller provides the native-code trust and ABI contract.
    let api = unsafe { ffi::Api::load(path) }.map_err(|_| Error::Unsupported)?;
    inspect(&api)
}
struct Open {
    config: Config,
    unknown: u64,
    last_timestamp: Option<u64>,
    epoch: u64,
    fault: Option<BackendFault>,
    last_status: Option<(i32, u8, u8)>,
}
pub struct KvaserDriver {
    api: ffi::Api,
    _claim: Claim,
    devices: BTreeMap<u32, Device>,
    opened: BTreeMap<u32, Open>,
}
impl KvaserDriver {
    /// # Safety
    /// Same contract as `discover`, with bounded setup/close and nonblocking
    /// reads/writes. Foreign calls cannot be preempted by the Rust worker.
    pub unsafe fn load(path: &Path, handles: &[u32]) -> Result<Self, Error> {
        // SAFETY: forwarded from caller.
        let api = unsafe { ffi::Api::load(path) }.map_err(|_| Error::Unsupported)?;
        let found = inspect(&api)?;
        let claim = api
            .library
            .claim(handles)
            .map_err(|_| Error::InvalidState)?;
        let mut devices = BTreeMap::new();
        for &handle in handles {
            let d = found
                .iter()
                .find(|d| d.handle == handle)
                .ok_or(Error::UnknownChannel)?;
            if !d.available {
                return Err(Error::InvalidState);
            }
            devices.insert(handle, d.clone());
        }
        Ok(Self {
            api,
            _claim: claim,
            devices,
            opened: BTreeMap::new(),
        })
    }
    fn known(&self, key: u32) -> Result<(), Error> {
        if self.devices.contains_key(&key) {
            Ok(())
        } else {
            Err(Error::UnknownChannel)
        }
    }
    fn stamp(open: &mut Open, time: u64) -> Stamp {
        if open.last_timestamp.is_some_and(|last| time < last) {
            open.epoch = open.epoch.saturating_add(1)
        }
        open.last_timestamp = Some(time);
        Stamp {
            raw_us: Some(time as u32),
            extended_us: Some(time),
            epoch: open.epoch,
            quality: Quality::Uncertain,
        }
    }
    fn diagnostic(classes: u32, data: [u8; 8], stamp: Stamp) -> Event {
        Event::Diagnostic(Diagnostic {
            received_at: tokio::time::Instant::now(),
            generation: 0,
            classes,
            data,
            overflow: false,
            stamp,
        })
    }
    fn decode(open: &mut Open, m: ffi::Message) -> Result<Event, Error> {
        if m.dlc > 15 || m.len > 64 {
            return Err(Error::InvalidFrame);
        }
        let stamp = Self::stamp(open, m.timestamp);
        if m.flags & 0xc0 != 0 {
            if m.len > 8 || m.id & 0xc0000000 != 0 {
                return Err(Error::InvalidFrame);
            }
            let mut data = [0; 8];
            data[..usize::from(m.len)].copy_from_slice(&m.data[..usize::from(m.len)]);
            if m.len >= 3 && data[2] & 0x40 != 0 {
                open.fault = Some(BackendFault::BusOff)
            }
            return Ok(Self::diagnostic(
                m.id | (u32::from(m.flags & 0xc0) << 24),
                data,
                stamp,
            ));
        }
        let id = CanId::new(m.id, m.flags & 2 != 0)?;
        let frame = if m.flags & 4 != 0 {
            if m.flags & 1 != 0
                || open.config.data_bitrate.is_none()
                || LENGTHS[usize::from(m.dlc)] != usize::from(m.len)
            {
                return Err(Error::InvalidFrame);
            }
            Frame::fd(id, m.data[..usize::from(m.len)].to_vec(), m.flags & 8 != 0)?
        } else {
            if m.flags & 0x18 != 0 {
                return Err(Error::InvalidFrame);
            }
            if m.flags & 1 != 0 {
                Frame::remote(id, m.dlc.min(8))?
            } else {
                if m.len != m.dlc.min(8) {
                    return Err(Error::InvalidFrame);
                }
                Frame::classic(id, m.data[..usize::from(m.len)].to_vec())?
            }
        };
        if m.flags & 0x20 != 0 {
            // Native acknowledgements do not expose an application request ID.
            // Keep these observable without fabricating a correlated TX record.
            let mut raw = [0; 8];
            raw[..4].copy_from_slice(&m.id.to_le_bytes());
            raw[4] = m.flags;
            raw[5] = m.dlc;
            return Ok(Self::diagnostic(0, raw, stamp));
        }
        Ok(Event::Frame {
            frame,
            metadata: FrameMetadata {
                stamp,
                esi: m.flags & 16 != 0,
                raw_dlc: Some(m.dlc),
            },
            received_at: tokio::time::Instant::now(),
            echo: false,
        })
    }
}
impl Driver for KvaserDriver {
    fn source_evidence(&self) -> Vec<canharbor_core::measurement::SourceEvidence> {
        vec![self.api.library.source_evidence()]
    }
    fn descriptors(&self) -> Vec<Descriptor> {
        self.devices
            .values()
            .map(|d| Descriptor {
                key: d.handle,
                label: format!("{} (unverified libKvaserUSB)", d.name),
                capabilities: Capabilities {
                    device_id: format!("kvaser-handle-{}", d.handle),
                    backend: "kvaser".into(),
                    simulated: false,
                    fd: d.fd,
                    listen_only: true,
                    timestamps: true,
                    nominal_timing: None,
                    data_timing: None,
                    fixed_nominal_bitrates: NOMINAL.to_vec(),
                    fixed_data_bitrates: if d.fd { DATA.to_vec() } else { vec![] },
                },
            })
            .collect()
    }
    fn start(
        &mut self,
        key: u32,
        config: Config,
        guard: &dyn Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.known(key)?;
        if self.opened.contains_key(&key) {
            return Err(Error::InvalidState);
        }
        if !NOMINAL.contains(&config.nominal_bitrate)
            || config
                .data_bitrate
                .is_some_and(|v| !self.devices[&key].fd || !DATA.contains(&v))
        {
            return Err(Error::Unsupported);
        }
        let opts = ffi::Options {
            bitrate: config.nominal_bitrate as i32,
            data_bitrate: config.data_bitrate.unwrap_or(2_000_000) as i32,
            fd: i32::from(config.data_bitrate.is_some()),
            listen_only: i32::from(config.listen_only),
            sample_point: 80,
            ..Default::default()
        };
        guard()?;
        // SAFETY: handle is reserved, options are live and exactly match retained ABI.
        checked(unsafe { (self.api.open)(key, &opts) })?;
        self.opened.insert(
            key,
            Open {
                config,
                unknown: 0,
                last_timestamp: None,
                epoch: 0,
                fault: None,
                last_status: None,
            },
        );
        guard()?;
        let mut rate = 0;
        let mut mode = 0;
        let mut caps = 0;
        // SAFETY: live scalar output buffers; optional timing pointers may be NULL per header.
        checked(unsafe {
            (self.api.parameters)(
                key,
                &mut rate,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        })?;
        if rate != opts.bitrate {
            return Err(Error::Unsupported);
        }
        guard()?;
        // SAFETY: channel is owned/open; mode output is valid for the call.
        let result = unsafe { (self.api.mode)(key, &mut mode) };
        if result == 0 {
            if mode != opts.listen_only {
                return Err(Error::Unsupported);
            }
        } else if result != -8 || opts.fd != 0 {
            return Err(error(result));
        }
        guard()?;
        // SAFETY: owned handle and valid output buffer.
        checked(unsafe { (self.api.capabilities)(key, &mut caps) })?;
        if opts.fd != 0 && caps & 1 == 0 {
            return Err(Error::Unsupported);
        }
        guard()
    }
    fn stop(&mut self, key: u32) -> Result<CloseSummary, Error> {
        self.known(key)?;
        let Some(open) = self.opened.remove(&key) else {
            return Ok(CloseSummary::default());
        };
        // SAFETY: only handles successfully opened by this instance are closed.
        // The void ABI cannot report bus-off or USB cleanup acknowledgement.
        unsafe { (self.api.close)(key) };
        Ok(CloseSummary {
            unknown_transmissions: open.unknown,
            failures: vec![],
        })
    }
    fn submit(
        &mut self,
        key: u32,
        frame: Frame,
        guard: &dyn Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        frame.validate()?;
        let open = self.opened.get_mut(&key).ok_or(Error::InvalidState)?;
        if open.config.listen_only
            || matches!(frame, Frame::FdData { .. }) && open.config.data_bitrate.is_none()
        {
            return Err(Error::Unsupported);
        }
        let mut message = ffi::Message {
            id: frame.id().value(),
            flags: u8::from(frame.id().extended()) * 2,
            dlc: frame.dlc(),
            len: frame.dlc(),
            ..Default::default()
        };
        if matches!(frame, Frame::FdData { .. }) {
            message.flags |= 4 | if matches!(frame, Frame::FdData { brs: true, .. }) {
                8
            } else {
                0
            };
            message.len = frame.data().len() as u8;
        } else if matches!(frame, Frame::ClassicRemote { .. }) {
            message.flags |= 1;
        } else {
            message.len = frame.data().len() as u8;
        }
        message.data[..frame.data().len()].copy_from_slice(frame.data());
        guard()?;
        // SAFETY: frame bounds validated by core, live repr(C) input, owned handle.
        let code = unsafe { (self.api.write)(key, &message) };
        if !matches!(code, -1 | -2 | -5 | -7 | -8 | -9 | -10) {
            open.unknown = open.unknown.saturating_add(1)
        }
        checked(code)
    }
    fn receive(&mut self, key: u32) -> Result<Option<Event>, Error> {
        let open = self.opened.get_mut(&key).ok_or(Error::InvalidState)?;
        if let Some(reason) = open.fault.take() {
            return Err(Error::Fault(reason));
        }
        let mut message = ffi::Message::default();
        // SAFETY: live output buffer with retained header layout.
        let code = unsafe { (self.api.read)(key, &mut message) };
        if code == 0 {
            return Self::decode(open, message).map(Some);
        }
        if code != -6 {
            return Err(error(code));
        }
        let (mut off, mut tec, mut rec) = (0, 0, 0);
        // SAFETY: live scalar outputs, only owned open channel queried.
        checked(unsafe { (self.api.status)(key, &mut off, &mut tec, &mut rec) })?;
        if open.last_status != Some((off, tec, rec)) {
            open.last_status = Some((off, tec, rec));
            if off != 0 {
                open.fault = Some(BackendFault::BusOff)
            }
            return Ok(Some(Self::diagnostic(
                0x80000000,
                [rec, tec, if off != 0 { 0x40 } else { 0 }, 0, 0, 0, 0, 0],
                Stamp::default(),
            )));
        }
        Ok(None)
    }
}
impl Drop for KvaserDriver {
    fn drop(&mut self) {
        let keys = self.opened.keys().copied().collect::<Vec<_>>();
        for key in keys {
            let _ = self.stop(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_driver::library::Library;
    use std::{
        path::PathBuf,
        process::Command,
        sync::{Arc, OnceLock},
    };
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    fn image() -> PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let root = std::env::temp_dir()
                .join(format!("canharbor-kvaser-mock-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let out = root.join(if cfg!(target_os = "macos") {
                "libmock.dylib"
            } else {
                "libmock.so"
            });
            let result = Command::new("cc")
                .arg(if cfg!(target_os = "macos") {
                    "-dynamiclib"
                } else {
                    "-shared"
                })
                .arg("-fPIC")
                .arg("-std=c11")
                .arg("-Werror")
                .arg("-I")
                .arg(directory.join("../../third-party/macOS-CAN-Drivers/KvaserUSB/include"))
                .arg(directory.join("tests/mock.c"))
                .arg("-o")
                .arg(&out)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            out
        })
        .clone()
    }
    fn symbol<T: Copy>(name: &std::ffi::CStr) -> T {
        unsafe { Library::load(&image()).unwrap().symbol(name).unwrap() }
    }
    fn reset() {
        let reset: unsafe extern "C" fn() = symbol(c"mock_reset");
        unsafe { reset() };
    }
    fn config(fd: bool) -> Config {
        Config {
            nominal_bitrate: 500_000,
            data_bitrate: fd.then_some(2_000_000),
            listen_only: false,
        }
    }
    fn driver() -> KvaserDriver {
        unsafe { KvaserDriver::load(&image(), &[0x10200, 0x10201]) }.unwrap()
    }
    fn fail(code: i32) {
        let f: unsafe extern "C" fn(i32) = symbol(c"mock_fail");
        unsafe { f(code) }
    }
    fn inject(handle: u32, flags: u32, dlc: u32, len: u32) {
        let f: unsafe extern "C" fn(u32, u32, u32, u32) = symbol(c"mock_frame");
        unsafe { f(handle, flags, dlc, len) }
    }
    #[test]
    fn layout_scan_and_shared_ownership_match_c_header() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let size: unsafe extern "C" fn(u32) -> usize = symbol(c"mock_size");
        let offset: unsafe extern "C" fn(u32) -> usize = symbol(c"mock_offset");
        unsafe {
            assert_eq!(size(0), std::mem::size_of::<ffi::Channel>());
            assert_eq!(size(1), std::mem::size_of::<ffi::Message>());
            assert_eq!(size(2), std::mem::size_of::<ffi::Options>());
            assert_eq!(offset(0), std::mem::offset_of!(ffi::Channel, serial));
            assert_eq!(offset(1), std::mem::offset_of!(ffi::Message, data));
            assert_eq!(offset(2), std::mem::offset_of!(ffi::Message, timestamp));
        }
        assert_eq!(unsafe { discover(&image()) }.unwrap().len(), 2);
        let d = driver();
        assert!(unsafe { KvaserDriver::load(&image(), &[0x10200]) }.is_err());
        drop(d);
        assert!(unsafe { KvaserDriver::load(&image(), &[0x10200]) }.is_ok());
    }
    #[test]
    fn fd_lengths_remote_quirk_and_timestamps_are_preserved() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut d = driver();
        d.start(0x10201, config(true), &|| Ok(())).unwrap();
        for (dlc, length) in LENGTHS.into_iter().enumerate() {
            inject(0x10201, 4 | 8 | 16, dlc as u32, length as u32);
            let event = d.receive(0x10201).unwrap().unwrap();
            assert!(
                matches!(event,Event::Frame{metadata,frame,..} if metadata.esi&&metadata.raw_dlc==Some(dlc as u8)&&metadata.stamp.extended_us==Some(0x123456789)&&frame.data().len()==length)
            );
        }
        inject(0x10201, 0, 15, 8);
        assert!(
            matches!(d.receive(0x10201).unwrap(),Some(Event::Frame{frame,metadata,..}) if frame.data().len()==8 && metadata.raw_dlc==Some(15))
        );
        inject(0x10201, 4, 9, 8);
        assert_eq!(d.receive(0x10201).err(), Some(Error::InvalidFrame));
        d.start(0x10200, config(false), &|| Ok(())).unwrap();
        let remote = Frame::remote(CanId::new(0x123, false).unwrap(), 8).unwrap();
        d.submit(0x10200, remote, &|| Ok(())).unwrap();
        let last: unsafe extern "C" fn(u32) -> u32 = symbol(c"mock_last");
        unsafe {
            assert_eq!(last(0), 1);
            assert_eq!(last(1), 8);
            assert_eq!(last(2), 8);
        }
        inject(0x10200, 1, 8, 8);
        assert!(matches!(
            d.receive(0x10200).unwrap(),
            Some(Event::Frame {
                frame: Frame::ClassicRemote { dlc: 8, .. },
                ..
            })
        ));
    }
    #[test]
    fn readback_and_revocation_fail_without_active_fallback() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut d = driver();
        fail(1);
        let mut c = config(true);
        c.listen_only = true;
        assert_eq!(
            d.start(0x10201, c, &|| Ok(())).err(),
            Some(Error::Unsupported)
        );
        d.stop(0x10201).unwrap();
        fail(2);
        assert!(d.start(0x10200, config(false), &|| Ok(())).is_err());
        d.stop(0x10200).unwrap();
        fail(0);
        assert!(
            d.start(0x10200, config(false), &|| Err(Error::InvalidState))
                .is_err()
        );
        // Classic family lacks firmware mode readback. Do not require a capability
        // bit which the SDK's product-table fallback omits.
        d.start(0x10200, config(false), &|| Ok(())).unwrap();
        let frame = Frame::classic(CanId::new(1, false).unwrap(), vec![]).unwrap();
        assert!(
            d.submit(0x10200, frame, &|| Err(Error::InvalidState))
                .is_err()
        );
        assert_eq!(d.stop(0x10200).unwrap().unknown_transmissions, 0);
    }
    #[test]
    fn unknown_writes_are_conservative_bounded_and_retired_once() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut d = driver();
        d.start(0x10200, config(false), &|| Ok(())).unwrap();
        let frame = Frame::classic(CanId::new(1, false).unwrap(), vec![1]).unwrap();
        for _ in 0..100 {
            d.submit(0x10200, frame.clone(), &|| Ok(())).unwrap();
        }
        fail(-10);
        assert_eq!(
            d.submit(0x10200, frame.clone(), &|| Ok(())).err(),
            Some(Error::QueueFull)
        );
        fail(-3);
        assert!(d.submit(0x10200, frame, &|| Ok(())).is_err());
        assert_eq!(d.stop(0x10200).unwrap().unknown_transmissions, 101);
        assert_eq!(d.stop(0x10200).unwrap().unknown_transmissions, 0);
        let count: unsafe extern "C" fn(i32) -> i32 = symbol(c"mock_count");
        unsafe {
            assert_eq!(count(1), 1);
            assert_eq!(count(0), 100);
        }
    }
    #[test]
    fn errors_and_uncorrelated_echoes_never_become_data() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut d = driver();
        d.start(0x10201, config(true), &|| Ok(())).unwrap();
        for flag in [0x40, 0x80, 0x24] {
            inject(0x10201, flag, 4, 4);
            assert!(matches!(
                d.receive(0x10201).unwrap(),
                Some(Event::Diagnostic(_))
            ));
        }
        let off: unsafe extern "C" fn(u32) = symbol(c"mock_busoff");
        unsafe { off(0x10201) };
        assert!(matches!(
            d.receive(0x10201).unwrap(),
            Some(Event::Diagnostic(_))
        ));
        assert_eq!(
            d.receive(0x10201).err(),
            Some(Error::Fault(BackendFault::BusOff))
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn worker_routes_two_channels_and_reports_unconfirmed_shutdown() {
        use canharbor_core::runtime::Runtime;
        use canharbor_driver::DriverBackend;
        let _serial = SERIAL.lock().await;
        reset();
        let backend = Arc::new(
            DriverBackend::open(|| Ok(Box::new(driver()) as Box<dyn Driver>), "test".into())
                .await
                .unwrap(),
        );
        let runtime = Runtime::new(backend);
        let a = runtime
            .create_configured("kvaser:test:66048", config(false))
            .await
            .unwrap();
        let b = runtime
            .create_configured("kvaser:test:66049", config(true))
            .await
            .unwrap();
        let ga = runtime.acquire(&a.id, "owner").await.unwrap().generation;
        let gb = runtime.acquire(&b.id, "owner").await.unwrap().generation;
        runtime.running(&a.id, "owner", ga, true).await.unwrap();
        runtime.running(&b.id, "owner", gb, true).await.unwrap();
        runtime
            .send(
                &a.id,
                "owner",
                ga,
                Frame::classic(CanId::new(1, false).unwrap(), vec![1]).unwrap(),
            )
            .await
            .unwrap();
        runtime
            .send(
                &b.id,
                "owner",
                gb,
                Frame::fd(CanId::new(2, false).unwrap(), vec![2; 12], true).unwrap(),
            )
            .await
            .unwrap();
        let summary = runtime.shutdown().await;
        assert_eq!(summary.unknown_transmissions, 2);
    }
    #[test]
    fn revocation_between_readbacks_skips_later_native_queries() {
        let _serial = SERIAL.blocking_lock();
        let queries: unsafe extern "C" fn(u32) -> u32 = symbol(c"mock_queries");
        for allowed in [2, 3] {
            reset();
            let mut d = driver();
            let calls = std::cell::Cell::new(0);
            let guard = || {
                let n = calls.get() + 1;
                calls.set(n);
                if n > allowed {
                    Err(Error::InvalidState)
                } else {
                    Ok(())
                }
            };
            assert_eq!(
                d.start(0x10201, config(true), &guard),
                Err(Error::InvalidState)
            );
            unsafe {
                assert_eq!(queries(0), 1);
                assert_eq!(queries(1), u32::from(allowed == 3));
                assert_eq!(queries(2), 0);
            }
            d.stop(0x10201).unwrap();
        }
    }
}
