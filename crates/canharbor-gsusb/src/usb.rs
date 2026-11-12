// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit device selection and descriptor-driven nusb transport.
use crate::{Error, control::Request, device::Transport};
use nusb::{
    MaybeFuture,
    descriptors::{ConfigurationDescriptor, TransferType},
    transfer::{Buffer, Bulk, Completion, ControlIn, ControlOut, ControlType, In, Out, Recipient},
};
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_millis(250);
const RX_BYTES: usize = 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VidPid {
    pub vendor: u16,
    pub product: u16,
}
pub fn allowed(id: VidPid, explicit: &[VidPid]) -> bool {
    explicit.contains(&id)
}
/// Enumeration does not open devices or send gs_usb requests. Every VID/PID
/// must be explicitly allowed; callers must then choose a candidate/interface.
pub fn discover(explicit: &[VidPid]) -> Result<Vec<Candidate>, Error> {
    if explicit.is_empty() {
        return Ok(Vec::new());
    }
    Ok(nusb::list_devices()
        .wait()
        .map_err(|_| Error::Transport)?
        .filter(|d| {
            allowed(
                VidPid {
                    vendor: d.vendor_id(),
                    product: d.product_id(),
                },
                explicit,
            )
        })
        .map(|info| Candidate { info })
        .collect())
}
pub struct Candidate {
    info: nusb::DeviceInfo,
}
impl Candidate {
    pub fn id(&self) -> VidPid {
        VidPid {
            vendor: self.info.vendor_id(),
            product: self.info.product_id(),
        }
    }
    /// May be absent or duplicated; never treated as a stable unique key.
    pub fn serial(&self) -> Option<&str> {
        self.info.serial_number()
    }
    pub fn open(self, interface: u8, alternate: u8) -> Result<UsbTransport, Error> {
        let device = self
            .info
            .open()
            .wait()
            .map_err(|e| access_error("open", &e))?;
        let selected = select_endpoints(
            &device
                .active_configuration()
                .map_err(|_| Error::Protocol("active USB configuration"))?,
            interface,
            alternate,
        )?;
        // Deliberately do not detach a kernel driver or change configuration.
        let claimed = device
            .claim_interface(interface)
            .wait()
            .map_err(|e| access_error("claim interface", &e))?;
        if claimed.get_alt_setting() != alternate {
            claimed
                .set_alt_setting(alternate)
                .wait()
                .map_err(|_| Error::Transport)?;
        }
        let input = claimed
            .endpoint::<Bulk, In>(selected.input)
            .map_err(|_| Error::Transport)?;
        let output = claimed
            .endpoint::<Bulk, Out>(selected.output)
            .map_err(|_| Error::Transport)?;
        Ok(UsbTransport {
            interface: claimed,
            input,
            output,
            poisoned: false,
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoints {
    pub input: u8,
    pub output: u8,
}
/// Require the selected vendor interface/alternate to have one bulk endpoint in
/// each direction. Ambiguity is rejected rather than guessing endpoint numbers.
pub fn select_endpoints(
    config: &ConfigurationDescriptor<'_>,
    interface: u8,
    alternate: u8,
) -> Result<Endpoints, Error> {
    let mut matches = config
        .interface_alt_settings()
        .filter(|d| d.interface_number() == interface && d.alternate_setting() == alternate);
    let descriptor = matches
        .next()
        .ok_or(Error::Protocol("USB interface/alternate"))?;
    if matches.next().is_some() || descriptor.class() != 0xff {
        return Err(Error::Unsupported("USB interface layout"));
    }
    if descriptor.endpoints().count() != usize::from(descriptor.num_endpoints()) {
        return Err(Error::Protocol("USB endpoint count"));
    }
    let mut input = None;
    let mut output = None;
    for ep in descriptor
        .endpoints()
        .filter(|e| e.transfer_type() == TransferType::Bulk)
    {
        let address = ep.address();
        if address & 0x70 != 0
            || address & 15 == 0
            || ep.max_packet_size() == 0
            || !RX_BYTES.is_multiple_of(ep.max_packet_size())
        {
            return Err(Error::Protocol("USB bulk endpoint"));
        }
        let slot = if address & 0x80 != 0 {
            &mut input
        } else {
            &mut output
        };
        if slot.replace(address).is_some() {
            return Err(Error::Unsupported("ambiguous bulk endpoints"));
        }
    }
    Ok(Endpoints {
        input: input.ok_or(Error::Unsupported("bulk IN"))?,
        output: output.ok_or(Error::Unsupported("bulk OUT"))?,
    })
}
/// The actor polls one shared IN endpoint; a timeout keeps its transfer queued.
/// Implementations must not block beyond the requested wait except OS scheduling.
pub trait ReceiveTransport: Transport {
    fn read(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, Error>;
    /// Cancel both directions and drain completions within one shared budget.
    /// Failed draining must not permit endpoint reuse.
    fn cancel(&mut self, budget: Duration) -> Result<(), Error>;
}
pub struct UsbTransport {
    interface: nusb::Interface,
    input: nusb::Endpoint<Bulk, In>,
    output: nusb::Endpoint<Bulk, Out>,
    poisoned: bool,
}
fn check_request(r: &Request, input: bool, interface: u8) -> Result<(), Error> {
    if r.input != input || r.index != u16::from(interface) {
        return Err(Error::Protocol("USB control setup"));
    }
    Ok(())
}
/// Strict completion validation, also exercised with synthetic nusb completions.
pub fn completed_read(c: Completion) -> Result<Vec<u8>, Error> {
    c.status.map_err(|_| Error::Transport)?;
    if c.actual_len != c.buffer.len() || c.actual_len > RX_BYTES {
        return Err(Error::Protocol("USB completion length"));
    }
    Ok(c.buffer.into_vec())
}
pub fn completed_write(c: Completion, expected: usize) -> Result<(), Error> {
    c.status.map_err(|_| Error::Transport)?;
    if c.actual_len != expected {
        return Err(Error::Protocol("short USB write"));
    }
    Ok(())
}
impl Transport for UsbTransport {
    fn control_in(&mut self, r: Request, length: usize) -> Result<Vec<u8>, Error> {
        check_request(&r, true, self.interface.interface_number())?;
        if self.poisoned {
            return Err(Error::Transport);
        }
        let length = u16::try_from(length).map_err(|_| Error::Protocol("control length"))?;
        self.interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Interface,
                    request: r.request,
                    value: r.value,
                    index: r.index,
                    length,
                },
                IO_TIMEOUT,
            )
            .wait()
            .map_err(|_| Error::Transport)
    }
    fn control_out(&mut self, r: Request, data: &[u8]) -> Result<(), Error> {
        check_request(&r, false, self.interface.interface_number())?;
        if self.poisoned {
            return Err(Error::Transport);
        }
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Interface,
                    request: r.request,
                    value: r.value,
                    index: r.index,
                    data,
                },
                IO_TIMEOUT,
            )
            .wait()
            .map_err(|_| Error::Transport)
    }
    fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Transport);
        }
        self.output.submit(data.to_vec().into());
        let Some(c) = self.output.wait_next_complete(IO_TIMEOUT) else {
            self.output.cancel_all();
            self.poisoned = true;
            return Err(Error::Transport);
        };
        let result = completed_write(c, data.len());
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
}
trait Pending {
    fn pending(&self) -> usize;
    fn cancel(&mut self);
    fn drain_one(&mut self, wait: Duration) -> bool;
}
impl<D: nusb::transfer::EndpointDirection> Pending for nusb::Endpoint<Bulk, D> {
    fn pending(&self) -> usize {
        self.pending()
    }
    fn cancel(&mut self) {
        self.cancel_all();
    }
    fn drain_one(&mut self, wait: Duration) -> bool {
        self.wait_next_complete(wait).is_some()
    }
}
fn cancel_pair(
    input: &mut impl Pending,
    output: &mut impl Pending,
    budget: Duration,
) -> Result<(), Error> {
    let started = std::time::Instant::now();
    input.cancel();
    output.cancel();
    for endpoint in [input as &mut dyn Pending, output as &mut dyn Pending] {
        while endpoint.pending() > 0 {
            let remaining = budget.saturating_sub(started.elapsed());
            if remaining.is_zero() || !endpoint.drain_one(remaining) {
                return Err(Error::Transport);
            }
        }
    }
    Ok(())
}
impl ReceiveTransport for UsbTransport {
    fn cancel(&mut self, budget: Duration) -> Result<(), Error> {
        self.poisoned = true;
        cancel_pair(&mut self.input, &mut self.output, budget)
    }
    fn read(&mut self, wait: Duration) -> Result<Option<Vec<u8>>, Error> {
        if self.poisoned {
            return Err(Error::Transport);
        }
        if self.input.pending() == 0 {
            self.input.submit(Buffer::new(RX_BYTES));
        }
        let Some(c) = self.input.wait_next_complete(wait) else {
            return Ok(None);
        };
        let result = completed_read(c);
        if result.is_err() {
            self.poisoned = true;
        }
        result.map(|bytes| if bytes.is_empty() { None } else { Some(bytes) })
    }
}

fn access_error(stage: &'static str, error: &impl std::fmt::Display) -> Error {
    let hint = if cfg!(windows) {
        "; Windows requires WinUSB on the selected gs_usb interface. Check the device/interface and its driver in Device Manager; CANHarbor never replaces drivers automatically"
    } else {
        ""
    };
    Error::UsbAccess {
        stage,
        detail: format!("{error}{hint}"),
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    struct Fake {
        pending: usize,
        cancelled: bool,
        complete: bool,
    }
    impl Pending for Fake {
        fn pending(&self) -> usize {
            self.pending
        }
        fn cancel(&mut self) {
            self.cancelled = true;
        }
        fn drain_one(&mut self, _: Duration) -> bool {
            if !self.complete {
                return false;
            }
            self.pending -= 1;
            true
        }
    }
    #[test]
    fn cancellation_precedes_draining_and_both_directions_are_drained() {
        let mut input = Fake {
            pending: 4,
            cancelled: false,
            complete: true,
        };
        let mut output = Fake {
            pending: 2,
            cancelled: false,
            complete: true,
        };
        cancel_pair(&mut input, &mut output, Duration::from_secs(1)).unwrap();
        assert!(input.cancelled && output.cancelled);
        assert_eq!(input.pending + output.pending, 0);
    }
    #[test]
    fn stalled_drain_or_exhausted_budget_still_cancels_both_endpoints() {
        for budget in [Duration::ZERO, Duration::from_millis(10)] {
            let mut input = Fake {
                pending: 1,
                cancelled: false,
                complete: false,
            };
            let mut output = Fake {
                pending: 1,
                cancelled: false,
                complete: true,
            };
            assert!(cancel_pair(&mut input, &mut output, budget).is_err());
            assert!(input.cancelled && output.cancelled);
            assert_eq!(input.pending, 1);
        }
    }
}
