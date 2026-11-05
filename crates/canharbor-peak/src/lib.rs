// SPDX-License-Identifier: AGPL-3.0-or-later
//! Optional adapter for the open-source libPeakUSB 1.0.2 C API.
mod ffi;
use canharbor_core::{
    BackendFault, CanId, Capabilities, CloseSummary, Config, Diagnostic, Error, Frame,
    FrameMetadata,
    timestamp::{Quality, Stamp},
};
use canharbor_driver::{Descriptor, Driver, Event, library::Claim};
use canharbor_gsusb::{control::Limits, timing};
use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
};
const NOMINAL: [(u32, u16); 10] = [
    (5_000, 0x7f7f),
    (10_000, 0x672f),
    (20_000, 0x532f),
    (50_000, 0x472f),
    (100_000, 0x432f),
    (125_000, 0x031c),
    (250_000, 0x011c),
    (500_000, 0x001c),
    (800_000, 0x0016),
    (1_000_000, 0x0014),
];
const DATA: [u32; 5] = [1_000_000, 2_000_000, 4_000_000, 5_000_000, 8_000_000];
const LENGTHS: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 20, 24, 32, 48, 64];
#[derive(Clone, Debug, serde::Serialize)]
pub struct Device {
    pub handle: u16,
    pub name: String,
    pub device_id: u32,
    pub fd: bool,
    pub available: bool,
}
fn error(status: u32) -> Error {
    if status & ffi::BUS_OFF != 0 {
        Error::Fault(BackendFault::BusOff)
    } else if status & ffi::OVERFLOW != 0 {
        Error::Fault(BackendFault::ReceiveOverflow)
    } else if status & ffi::TX_FULL != 0 {
        Error::QueueFull
    } else if status & (0x400 | 0x800) != 0 {
        Error::InvalidState
    } else if status & (0x4000 | 0x8000 | 0x20000 | 0x80000 | 0x8000000) != 0 {
        Error::Unsupported
    } else {
        Error::Fault(BackendFault::Disconnected)
    }
}
fn checked(status: u32) -> Result<(), Error> {
    if status == 0 {
        Ok(())
    } else {
        Err(error(status))
    }
}
fn word(api: &ffi::Api, handle: u16, parameter: u8) -> Result<u32, Error> {
    let mut value = 0u32;
    // SAFETY: Api was loaded under its ABI contract; this buffer has the size
    // passed to the selected DWORD parameter.
    checked(unsafe { (api.get)(handle, parameter, (&mut value as *mut u32).cast(), 4) })?;
    Ok(value)
}
fn set_word(
    api: &ffi::Api,
    handle: u16,
    parameter: u8,
    value: u32,
    guard: &dyn Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    guard()?;
    let mut value = value;
    // SAFETY: the input buffer is live for this synchronous call and is four bytes.
    checked(unsafe { (api.set)(handle, parameter, (&mut value as *mut u32).cast(), 4) })
}
fn inspect(api: &ffi::Api) -> Result<Vec<Device>, Error> {
    let mut version = [0u8; 64];
    // PCAN_CHANNEL_VERSION on PCAN_NONEBUS identifies libPeakUSB itself.
    // PCAN_API_VERSION instead reports the PCAN-Basic API version (4.9.0).
    // SAFETY: the version parameter writes at most the supplied buffer length.
    checked(unsafe { (api.get)(0, 0x06, version.as_mut_ptr().cast(), version.len() as u32) })?;
    if version.split(|byte| *byte == 0).next() != Some(b"1.0.2".as_slice()) {
        return Err(Error::Unsupported);
    }
    let count = word(api, 0, 0x2a)? as usize;
    if count > 16 {
        return Err(Error::Unsupported);
    }
    if count == 0 {
        return Ok(vec![]);
    }
    let mut info = vec![ffi::ChannelInfo::default(); count];
    // SAFETY: the retained header defines the exact repr(C) layout. The driver
    // receives the array's full byte size and rejects growth beyond that bound.
    checked(unsafe {
        (api.get)(
            0,
            0x2b,
            info.as_mut_ptr().cast(),
            std::mem::size_of_val(info.as_slice()) as u32,
        )
    })?;
    let mut devices = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for info in info {
        if info.device != 5 || info.handle == 0 || !seen.insert(info.handle) {
            return Err(Error::Unsupported);
        }
        let end = info
            .name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(info.name.len());
        devices.push(Device {
            handle: info.handle,
            name: String::from_utf8_lossy(&info.name[..end]).into_owned(),
            device_id: info.device_id,
            fd: info.features & 1 != 0,
            available: info.condition == 1,
        });
    }
    Ok(devices)
}
/// Inspect a trusted open-source libPeakUSB build without initializing channels.
///
/// # Safety
/// The path must refer to a trusted libPeakUSB 1.0.2 implementation of the retained
/// C ABI. Its constructors, pointer contracts, and thread synchronization apply.
pub unsafe fn discover(path: &Path) -> Result<Vec<Device>, Error> {
    // SAFETY: forwarded from this function's caller.
    let api = unsafe { ffi::Api::load(path) }.map_err(|_| Error::Unsupported)?;
    inspect(&api)
}
struct Open {
    config: Config,
    pending: VecDeque<Frame>,
    uncertain: u64,
    events: VecDeque<Event>,
    last_status: u32,
    last_timestamp: Option<u64>,
    epoch: u64,
    fault: Option<BackendFault>,
}
pub struct PeakDriver {
    api: ffi::Api,
    _claim: Claim,
    devices: BTreeMap<u16, Device>,
    opened: BTreeMap<u16, Open>,
}
impl PeakDriver {
    /// # Safety
    /// Same native-code and ABI contract as `discover`. The selected library must
    /// provide bounded setup/teardown and nonblocking read/write operations.
    pub unsafe fn load(path: &Path, handles: &[u16]) -> Result<Self, Error> {
        // SAFETY: the caller selects a trusted implementation of this exact ABI.
        let api = unsafe { ffi::Api::load(path) }.map_err(|_| Error::Unsupported)?;
        let available = inspect(&api)?;
        let keys = handles
            .iter()
            .map(|&key| u32::from(key))
            .collect::<Vec<_>>();
        let claim = api.library.claim(&keys).map_err(|_| Error::InvalidState)?;
        let mut devices = BTreeMap::new();
        for &handle in handles {
            let device = available
                .iter()
                .find(|device| device.handle == handle)
                .ok_or(Error::UnknownChannel)?;
            if !device.available {
                return Err(Error::InvalidState);
            }
            devices.insert(handle, device.clone());
        }
        Ok(Self {
            api,
            _claim: claim,
            devices,
            opened: BTreeMap::new(),
        })
    }
    fn handle(&self, key: u32) -> Result<u16, Error> {
        let handle = u16::try_from(key).map_err(|_| Error::UnknownChannel)?;
        if self.devices.contains_key(&handle) {
            Ok(handle)
        } else {
            Err(Error::UnknownChannel)
        }
    }
    fn fd_timing(config: Config) -> Result<Vec<u8>, Error> {
        let nominal = Limits {
            tseg1_min: 1,
            tseg1_max: 256,
            tseg2_min: 1,
            tseg2_max: 128,
            sjw_max: 128,
            brp_min: 1,
            brp_max: 1024,
            brp_inc: 1,
        };
        let data = Limits {
            tseg1_max: 32,
            tseg2_max: 16,
            sjw_max: 16,
            ..nominal
        };
        let nominal = timing::solve(80_000_000, config.nominal_bitrate, 875, nominal)
            .map_err(|_| Error::Unsupported)?
            .timing;
        let data = timing::solve(
            80_000_000,
            config.data_bitrate.ok_or(Error::Unsupported)?,
            800,
            data,
        )
        .map_err(|_| Error::Unsupported)?
        .timing;
        let text = format!(
            "f_clock=80000000,nom_brp={},nom_tseg1={},nom_tseg2={},nom_sjw={},data_brp={},data_tseg1={},data_tseg2={},data_sjw={}",
            nominal.brp,
            nominal.prop_seg + nominal.phase_seg1,
            nominal.phase_seg2,
            nominal.sjw,
            data.brp,
            data.prop_seg + data.phase_seg1,
            data.phase_seg2,
            data.sjw
        );
        let mut bytes = text.into_bytes();
        bytes.push(0);
        Ok(bytes)
    }
    fn diagnostic(
        status: u32,
        flags: u8,
        data: &[u8],
        overflow: bool,
        timestamp: Option<u64>,
    ) -> Event {
        let mut raw = [0; 8];
        let length = data.len().min(8);
        raw[..length].copy_from_slice(&data[..length]);
        Event::Diagnostic(Diagnostic {
            received_at: tokio::time::Instant::now(),
            generation: 0,
            classes: status | u32::from(flags & (ffi::ERROR | ffi::STATUS)) << 24,
            data: raw,
            overflow,
            stamp: timestamp
                .map(|timestamp| Stamp {
                    raw_us: Some(timestamp as u32),
                    extended_us: Some(timestamp),
                    epoch: 0,
                    quality: Quality::Uncertain,
                })
                .unwrap_or_default(),
        })
    }
    fn decoded(
        open: &mut Open,
        id: u32,
        flags: u8,
        dlc: u8,
        data: &[u8],
        timestamp: u64,
    ) -> Result<Event, Error> {
        if flags & (ffi::ERROR | ffi::STATUS) != 0 {
            if id & 0xc0000000 != 0 || dlc > 8 {
                return Err(Error::InvalidFrame);
            }
            let status_frame = flags & ffi::STATUS != 0;
            let overflow = status_frame && id & ffi::OVERFLOW != 0;
            if status_frame && id & ffi::BUS_OFF != 0 {
                open.fault = Some(BackendFault::BusOff);
            } else if overflow {
                open.fault = Some(BackendFault::ReceiveOverflow);
            }
            return Ok(Self::diagnostic(id, flags, data, overflow, Some(timestamp)));
        }
        let id = CanId::new(id, flags & ffi::EXT != 0)?;
        let frame = if flags & ffi::FD != 0 {
            if flags & ffi::RTR != 0 || dlc > 15 || open.config.data_bitrate.is_none() {
                return Err(Error::InvalidFrame);
            }
            let length = LENGTHS[usize::from(dlc)];
            if data.len() < length {
                return Err(Error::InvalidFrame);
            }
            Frame::fd(id, data[..length].to_vec(), flags & ffi::BRS != 0)?
        } else {
            if dlc > 8 || flags & (ffi::BRS | ffi::ESI) != 0 {
                return Err(Error::InvalidFrame);
            }
            if flags & ffi::RTR != 0 {
                Frame::remote(id, dlc)?
            } else {
                let length = usize::from(dlc);
                if data.len() < length {
                    return Err(Error::InvalidFrame);
                }
                Frame::classic(id, data[..length].to_vec())?
            }
        };
        let echo = flags & ffi::ECHO != 0;
        if echo {
            if let Some(position) = open.pending.iter().position(|pending| pending == &frame) {
                open.pending.remove(position);
            } else {
                let mut raw = [0; 8];
                raw[..4].copy_from_slice(&frame.id().value().to_le_bytes());
                raw[4] = flags;
                raw[5] = dlc;
                return Ok(Self::diagnostic(0, flags, &raw, false, Some(timestamp)));
            }
        }
        if open.last_timestamp.is_some_and(|last| timestamp < last) {
            open.epoch = open.epoch.saturating_add(1);
        }
        open.last_timestamp = Some(timestamp);
        Ok(Event::Frame {
            metadata: FrameMetadata {
                stamp: Stamp {
                    raw_us: Some(timestamp as u32),
                    extended_us: Some(timestamp),
                    epoch: open.epoch,
                    quality: Quality::Uncertain,
                },
                esi: flags & ffi::ESI != 0,
                raw_dlc: open.config.data_bitrate.is_some().then_some(dlc),
            },
            frame,
            received_at: tokio::time::Instant::now(),
            echo,
        })
    }
}
impl Driver for PeakDriver {
    fn descriptors(&self) -> Vec<Descriptor> {
        self.devices
            .values()
            .map(|device| Descriptor {
                key: u32::from(device.handle),
                label: format!(
                    "{} (PEAK handle 0x{:X}, hardware unverified)",
                    device.name, device.handle
                ),
                capabilities: Capabilities {
                    device_id: format!("peak-handle-{}", device.handle),
                    backend: "peak".into(),
                    simulated: false,
                    fd: device.fd,
                    listen_only: true,
                    timestamps: true,
                    nominal_timing: None,
                    data_timing: None,
                    fixed_nominal_bitrates: NOMINAL.iter().map(|entry| entry.0).collect(),
                    fixed_data_bitrates: if device.fd { DATA.to_vec() } else { vec![] },
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
        let handle = self.handle(key)?;
        if self.opened.contains_key(&handle) {
            return Err(Error::InvalidState);
        }
        let bitrate = NOMINAL
            .iter()
            .find(|entry| entry.0 == config.nominal_bitrate)
            .ok_or(Error::Unsupported)?
            .1;
        let mut fd = if let Some(data) = config.data_bitrate {
            if !self.devices[&handle].fd || !DATA.contains(&data) {
                return Err(Error::Unsupported);
            }
            Some(Self::fd_timing(config)?)
        } else {
            None
        };
        set_word(
            &self.api,
            handle,
            0x08,
            u32::from(config.listen_only),
            guard,
        )?;
        set_word(&self.api, handle, 0x07, 0, guard)?;
        set_word(&self.api, handle, 0x2c, 1, guard)?;
        guard()?;
        // SAFETY: the selected handle is reserved by this adapter. The mutable
        // FD string remains alive throughout the synchronous initialization.
        let status = unsafe {
            if let Some(text) = &mut fd {
                (self.api.initialize_fd)(handle, text.as_mut_ptr().cast())
            } else {
                (self.api.initialize)(handle, bitrate, 0, 0, 0)
            }
        };
        if status == 0 || status & 0x2000000 != 0 {
            self.opened.insert(
                handle,
                Open {
                    config,
                    pending: VecDeque::new(),
                    uncertain: 0,
                    events: VecDeque::new(),
                    last_status: 0,
                    last_timestamp: None,
                    epoch: 0,
                    fault: None,
                },
            );
        }
        checked(status)?;
        guard()?;
        if word(&self.api, handle, 0x08)? != u32::from(config.listen_only)
            || word(&self.api, handle, 0x1a)? != config.nominal_bitrate
        {
            return Err(Error::Fault(BackendFault::StopFailed));
        }
        if let Some(data) = config.data_bitrate
            && word(&self.api, handle, 0x1b)? != data
        {
            return Err(Error::Fault(BackendFault::StopFailed));
        }
        Ok(())
    }
    fn stop(&mut self, key: u32) -> Result<CloseSummary, Error> {
        let handle = self.handle(key)?;
        let Some(open) = self.opened.get_mut(&handle) else {
            return Ok(CloseSummary::default());
        };
        let unknown = (open.pending.len() as u64).saturating_add(open.uncertain);
        open.pending.clear();
        open.uncertain = 0;
        // SAFETY: only this adapter's initialized handle is released. Handle zero,
        // which means all channels in PCAN-Basic, is never used for teardown.
        let status = unsafe { (self.api.uninitialize)(handle) };
        let success = status == 0 || status == 0x4000000;
        if success {
            self.opened.remove(&handle);
        }
        Ok(CloseSummary {
            unknown_transmissions: unknown,
            failures: if success {
                vec![]
            } else {
                vec![format!(
                    "PEAK handle 0x{handle:X} cleanup status 0x{status:X}"
                )]
            },
        })
    }
    fn submit(
        &mut self,
        key: u32,
        frame: Frame,
        guard: &dyn Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        let handle = self.handle(key)?;
        frame.validate()?;
        let open = self.opened.get_mut(&handle).ok_or(Error::InvalidState)?;
        if open.config.listen_only {
            return Err(Error::InvalidState);
        }
        let echo_supported = self.devices[&handle].fd;
        if open.events.len() >= 64 || (echo_supported && open.pending.len() >= 32) {
            return Err(Error::QueueFull);
        }
        let fd = matches!(frame, Frame::FdData { .. });
        if fd && open.config.data_bitrate.is_none() {
            return Err(Error::Unsupported);
        }
        let mut flags = if frame.id().extended() { ffi::EXT } else { 0 };
        if matches!(frame, Frame::ClassicRemote { .. }) {
            flags |= ffi::RTR;
        }
        if let Frame::FdData { brs, .. } = &frame {
            flags |= ffi::FD;
            if *brs {
                flags |= ffi::BRS;
            }
        }
        guard()?;
        // SAFETY: validated payload lengths fit these initialized repr(C) buffers.
        // In FD controller mode the API also carries Classic frames in MessageFd.
        let status = unsafe {
            if open.config.data_bitrate.is_some() {
                let mut message = ffi::MessageFd {
                    id: frame.id().value(),
                    flags,
                    dlc: frame.dlc(),
                    ..Default::default()
                };
                message.data[..frame.data().len()].copy_from_slice(frame.data());
                (self.api.write_fd)(handle, &mut message)
            } else {
                let mut message = ffi::Message {
                    id: frame.id().value(),
                    flags,
                    len: frame.dlc(),
                    ..Default::default()
                };
                message.data[..frame.data().len()].copy_from_slice(frame.data());
                (self.api.write)(handle, &mut message)
            }
        };
        if status & ffi::BUS_OFF != 0 {
            open.uncertain = open.uncertain.saturating_add(1);
            return Err(Error::Fault(BackendFault::BusOff));
        }
        if status & ffi::TX_FULL != 0 && status & ffi::BUS_OFF == 0 {
            return Err(Error::QueueFull);
        }
        if status & !(ffi::BUS_ERRORS) == 0 {
            if echo_supported {
                open.pending.push_back(frame);
            } else {
                open.uncertain = open.uncertain.saturating_add(1);
            }
            if status != open.last_status {
                open.last_status = status;
                open.events.push_back(Self::diagnostic(
                    status,
                    0,
                    &status.to_le_bytes(),
                    false,
                    None,
                ));
            }
            return Ok(());
        }
        open.uncertain = open.uncertain.saturating_add(1);
        Err(error(status))
    }
    fn receive(&mut self, key: u32) -> Result<Option<Event>, Error> {
        let handle = self.handle(key)?;
        let open = self.opened.get_mut(&handle).ok_or(Error::InvalidState)?;
        if let Some(event) = open.events.pop_front() {
            return Ok(Some(event));
        }
        if let Some(reason) = open.fault.take() {
            return Err(Error::Fault(reason));
        }
        let (status, id, flags, dlc, data, timestamp) = if open.config.data_bitrate.is_some() {
            let mut message = ffi::MessageFd::default();
            let mut timestamp = 0u64;
            // SAFETY: the driver writes only the retained header's message and timestamp sizes.
            let status = unsafe { (self.api.read_fd)(handle, &mut message, &mut timestamp) };
            (
                status,
                message.id,
                message.flags,
                message.dlc,
                message.data,
                timestamp,
            )
        } else {
            let mut message = ffi::Message::default();
            let mut stamp = ffi::Timestamp::default();
            // SAFETY: the message and timestamp buffers exactly match the C ABI.
            let status = unsafe { (self.api.read)(handle, &mut message, &mut stamp) };
            if stamp.micros >= 1000 {
                return Err(Error::InvalidFrame);
            }
            let timestamp = (u64::from(stamp.overflow) << 32 | u64::from(stamp.millis)) * 1000
                + u64::from(stamp.micros);
            let mut data = [0; 64];
            data[..8].copy_from_slice(&message.data);
            (
                status,
                message.id,
                message.flags,
                message.len,
                data,
                timestamp,
            )
        };
        if status & !(ffi::BUS_ERRORS | ffi::OVERFLOW | ffi::EMPTY) != 0 {
            return Err(error(status));
        }
        let bus = if status & ffi::EMPTY != 0 {
            // SAFETY: status queries are nonmutating and use an owned open handle.
            status | unsafe { (self.api.get_status)(handle) }
        } else {
            status
        };
        if bus & !(ffi::BUS_ERRORS | ffi::OVERFLOW | ffi::EMPTY) != 0 {
            return Err(error(bus));
        }
        if bus & ffi::BUS_OFF != 0 {
            open.fault = Some(BackendFault::BusOff);
        }
        let overflow = bus & ffi::OVERFLOW != 0;
        let relevant = bus & !ffi::EMPTY;
        if relevant != open.last_status {
            open.last_status = relevant;
            open.events.push_back(Self::diagnostic(
                relevant,
                0,
                &bus.to_le_bytes(),
                overflow,
                (status & ffi::EMPTY == 0).then_some(timestamp),
            ));
        }
        if status & ffi::EMPTY == 0 && bus & ffi::BUS_OFF == 0 && !overflow {
            let event = Self::decoded(open, id, flags, dlc, &data, timestamp)?;
            open.events.push_back(event);
        }
        if let Some(event) = open.events.pop_front() {
            Ok(Some(event))
        } else if let Some(reason) = open.fault.take() {
            Err(Error::Fault(reason))
        } else {
            Ok(None)
        }
    }
}
impl Drop for PeakDriver {
    fn drop(&mut self) {
        let keys = self.opened.keys().copied().collect::<Vec<_>>();
        for handle in keys {
            let _ = self.stop(u32::from(handle));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canharbor_core::runtime::Runtime;
    use canharbor_driver::{DriverBackend, library::Library};
    use std::{
        path::PathBuf,
        process::Command,
        sync::{Arc, OnceLock},
    };
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    fn image() -> PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            let root =
                std::env::temp_dir().join(format!("canharbor-peak-mock-{}", uuid::Uuid::new_v4()));
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
                .arg(directory.join("../../third-party/macOS-CAN-Drivers/PeakUSB/include"))
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
    fn option(key: u32, value: u32) {
        let call: unsafe extern "C" fn(u32, u32) = symbol(c"mock_option");
        unsafe { call(key, value) };
    }
    fn opened(handle: u32) -> u32 {
        let call: unsafe extern "C" fn(u32) -> u32 = symbol(c"mock_open");
        unsafe { call(handle) }
    }
    fn frame() -> Frame {
        Frame::classic(CanId::new(0x654, false).unwrap(), vec![1, 2]).unwrap()
    }
    fn config(fd: bool, listen_only: bool) -> Config {
        Config {
            nominal_bitrate: 500_000,
            data_bitrate: fd.then_some(2_000_000),
            listen_only,
        }
    }
    #[test]
    fn discovery_checks_library_version_separately_from_pcan_api_version() {
        let _serial = SERIAL.blocking_lock();
        reset();
        assert_eq!(unsafe { discover(&image()) }.unwrap().len(), 2);
        option(5, 1);
        assert!(matches!(
            unsafe { discover(&image()) },
            Err(Error::Unsupported)
        ));
        reset();
    }
    #[test]
    fn abi_layout_matches_the_retained_c_header_and_shared_channel_claims() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let size: unsafe extern "C" fn(u32) -> usize = symbol(c"mock_size");
        let offset: unsafe extern "C" fn(u32) -> usize = symbol(c"mock_offset");
        for (index, (length, field)) in [
            (
                std::mem::size_of::<ffi::Message>(),
                std::mem::offset_of!(ffi::Message, data),
            ),
            (
                std::mem::size_of::<ffi::Timestamp>(),
                std::mem::offset_of!(ffi::Timestamp, micros),
            ),
            (
                std::mem::size_of::<ffi::MessageFd>(),
                std::mem::offset_of!(ffi::MessageFd, data),
            ),
            (
                std::mem::size_of::<ffi::ChannelInfo>(),
                std::mem::offset_of!(ffi::ChannelInfo, device_id),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(length, unsafe { size(index as u32) });
            assert_eq!(field, unsafe { offset(index as u32) });
        }
        let first = unsafe { PeakDriver::load(&image(), &[0x51]) }.unwrap();
        assert!(unsafe { PeakDriver::load(&image(), &[0x51]) }.is_err());
        let alias = image().with_file_name("alias-image");
        std::fs::hard_link(image(), &alias).unwrap();
        assert!(unsafe { PeakDriver::load(&alias, &[0x51]) }.is_err());
        std::fs::remove_file(alias).unwrap();
        let second = unsafe { PeakDriver::load(&image(), &[0x52]) }.unwrap();
        drop(first);
        drop(second);
        assert!(unsafe { PeakDriver::load(&image(), &[0x51]) }.is_ok());
        let library = unsafe { Library::load(&image()) }.unwrap();
        assert!(unsafe { library.symbol::<unsafe extern "C" fn()>(c"missing_symbol") }.is_err());
        assert!(unsafe { Library::load(Path::new("relative.dylib")) }.is_err());
    }
    #[test]
    fn classic_fd_remote_metadata_and_driver_echo_do_not_claim_bus_ack() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut driver = unsafe { PeakDriver::load(&image(), &[0x51, 0x52]) }.unwrap();
        driver
            .start(0x51, config(false, false), &|| Ok(()))
            .unwrap();
        driver.start(0x52, config(true, false), &|| Ok(())).unwrap();
        driver.submit(0x51, frame(), &|| Ok(())).unwrap();
        assert!(driver.receive(0x51).unwrap().is_none());
        for length in LENGTHS {
            let frame =
                Frame::fd(CanId::new(0x123abcd, true).unwrap(), vec![7; length], true).unwrap();
            driver.submit(0x52, frame.clone(), &|| Ok(())).unwrap();
            assert!(
                matches!(driver.receive(0x52).unwrap(),Some(Event::Frame{frame:received,echo:true,metadata,..}) if received==frame && metadata.stamp.quality==Quality::Uncertain)
            );
        }
        let remote = Frame::remote(CanId::new(0x654, false).unwrap(), 8).unwrap();
        driver.submit(0x52, remote.clone(), &|| Ok(())).unwrap();
        assert!(
            matches!(driver.receive(0x52).unwrap(),Some(Event::Frame{frame,..}) if frame==remote)
        );
        assert_eq!(driver.stop(0x51).unwrap().unknown_transmissions, 1);
        assert_eq!(driver.stop(0x52).unwrap().unknown_transmissions, 0);
    }
    #[test]
    fn rejection_readback_revocation_and_unacknowledged_close_are_observable() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut driver = unsafe { PeakDriver::load(&image(), &[0x51]) }.unwrap();
        option(2, 1);
        assert!(driver.start(0x51, config(false, true), &|| Ok(())).is_err());
        assert_eq!(opened(0x51), 0);
        option(2, 0);
        assert!(
            driver
                .start(0x51, config(false, false), &|| Err(Error::ControlExpired))
                .is_err()
        );
        assert_eq!(opened(0x51), 0);
        option(3, 1);
        assert!(
            driver
                .start(0x51, config(false, false), &|| Ok(()))
                .is_err()
        );
        driver.stop(0x51).unwrap();
        option(3, 0);
        driver
            .start(0x51, config(false, false), &|| Ok(()))
            .unwrap();
        option(4, 0x80);
        assert_eq!(
            driver.submit(0x51, frame(), &|| Ok(())),
            Err(Error::QueueFull)
        );
        option(4, 0);
        driver.submit(0x51, frame(), &|| Ok(())).unwrap();
        option(1, 1);
        let report = driver.stop(0x51).unwrap();
        assert_eq!(report.unknown_transmissions, 1);
        assert!(!report.failures.is_empty());
        assert_eq!(driver.stop(0x51).unwrap().unknown_transmissions, 0);
        option(1, 0);
        driver.stop(0x51).unwrap();
    }
    #[test]
    fn classic_without_echo_has_no_artificial_lifetime_window_and_unmatched_echo_is_not_data() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut driver = unsafe { PeakDriver::load(&image(), &[0x51, 0x52]) }.unwrap();
        driver
            .start(0x51, config(false, false), &|| Ok(()))
            .unwrap();
        for _ in 0..100 {
            driver.submit(0x51, frame(), &|| Ok(())).unwrap();
        }
        assert_eq!(driver.stop(0x51).unwrap().unknown_transmissions, 100);
        driver.start(0x52, config(true, false), &|| Ok(())).unwrap();
        let inject: unsafe extern "C" fn(u32, u32, u32) = symbol(c"mock_frame");
        unsafe { inject(0x52, 0x24, 8) };
        assert!(matches!(
            driver.receive(0x52).unwrap(),
            Some(Event::Diagnostic(_))
        ));
        driver.stop(0x52).unwrap();
    }
    #[test]
    fn status_and_error_records_are_diagnostics_and_busoff_is_a_fault() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut driver = unsafe { PeakDriver::load(&image(), &[0x52]) }.unwrap();
        driver.start(0x52, config(true, false), &|| Ok(())).unwrap();
        let inject: unsafe extern "C" fn(u32, u32, u32) = symbol(c"mock_frame");
        unsafe { inject(0x52, 0x44, 8) };
        assert!(matches!(
            driver.receive(0x52).unwrap(),
            Some(Event::Diagnostic(_))
        ));
        let identifier: unsafe extern "C" fn(u32, u32) = symbol(c"mock_identifier");
        unsafe {
            inject(0x52, 0x80, 0);
            identifier(0x52, 0x40);
        }
        assert!(
            matches!(driver.receive(0x52).unwrap(),Some(Event::Diagnostic(diagnostic)) if diagnostic.classes==0x80000040 && diagnostic.overflow)
        );
        assert_eq!(
            driver.receive(0x52).err(),
            Some(Error::Fault(BackendFault::ReceiveOverflow))
        );
        let status: unsafe extern "C" fn(u32, u32) = symbol(c"mock_status");
        unsafe { status(0x52, 0x10) };
        assert!(matches!(
            driver.receive(0x52).unwrap(),
            Some(Event::Diagnostic(_))
        ));
        assert_eq!(
            driver.receive(0x52).err(),
            Some(Error::Fault(BackendFault::BusOff))
        );
        driver.stop(0x52).unwrap();
    }
    #[test]
    fn empty_reads_preserve_transient_overflow_before_status_query() {
        let _serial = SERIAL.blocking_lock();
        reset();
        let mut driver = unsafe { PeakDriver::load(&image(), &[0x51, 0x52]) }.unwrap();
        let transient: unsafe extern "C" fn(u32, u32) = symbol(c"mock_empty_status");
        for (handle, fd) in [(0x51, false), (0x52, true)] {
            driver.start(handle, config(fd, false), &|| Ok(())).unwrap();
            // The read reports loss, but the subsequent status query is healthy.
            unsafe { transient(handle, 0x40) };
            assert!(matches!(driver.receive(handle).unwrap(),
                Some(Event::Diagnostic(diagnostic)) if diagnostic.overflow && diagnostic.classes == 0x40));
            assert!(matches!(driver.receive(handle).unwrap(),
                Some(Event::Diagnostic(diagnostic)) if !diagnostic.overflow && diagnostic.classes == 0));
            assert!(driver.receive(handle).unwrap().is_none());
            driver.stop(handle).unwrap();
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn native_worker_runs_two_channels_and_closes_partial_startup() {
        let _serial = SERIAL.lock().await;
        reset();
        let backend = Arc::new(
            DriverBackend::open(
                || unsafe {
                    PeakDriver::load(&image(), &[0x51, 0x52])
                        .map(|driver| Box::new(driver) as Box<dyn Driver>)
                },
                "mock-epoch".into(),
            )
            .await
            .unwrap(),
        );
        let runtime = Runtime::new(backend.clone());
        let a = runtime
            .create_configured("peak:mock-epoch:81", config(false, true))
            .await
            .unwrap();
        let b = runtime
            .create_configured("peak:mock-epoch:82", config(true, false))
            .await
            .unwrap();
        let ga = runtime.acquire(&a.id, "owner").await.unwrap().generation;
        let gb = runtime.acquire(&b.id, "owner").await.unwrap().generation;
        runtime.running(&a.id, "owner", ga, true).await.unwrap();
        runtime.running(&b.id, "owner", gb, true).await.unwrap();
        let mut events = runtime.subscribe(&b.id).await.unwrap();
        runtime
            .send(
                &b.id,
                "owner",
                gb,
                Frame::fd(CanId::new(0x654, false).unwrap(), vec![1; 12], true).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap()
                .direction,
            "tx"
        );
        assert!(runtime.send(&a.id, "owner", ga, frame()).await.is_err());
        assert!(runtime.shutdown().await.failures.is_empty());
        assert_eq!(opened(0x51), 0);
        assert_eq!(opened(0x52), 0);
    }
}
