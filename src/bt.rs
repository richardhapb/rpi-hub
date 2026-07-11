//! Presenting the Pi to a host as a Bluetooth HID keyboard.
//!
//! Shape of the thing, in Bluetooth terms:
//!
//! * BlueZ publishes an **SDP record** advertising the HID profile (UUID 0x1124)
//!   and carrying our report map. That is what makes the host believe a keyboard
//!   is out there.
//! * The host then opens **two L2CAP channels**: PSM 17 (control) and PSM 19
//!   (interrupt). Key reports go out on the interrupt channel.
//! * We own those sockets ourselves rather than letting BlueZ's `Profile1` hand
//!   us one, because HID needs two channels and `Profile1` only surfaces one.
//!
//! Prerequisites on the Pi, both of which the systemd unit takes care of:
//! `bluetoothd --noplugin=input` (otherwise BlueZ claims the HID *host* role and
//! the PSMs are already taken), and Class-of-Device set to keyboard.

use anyhow::{Context, Result};
use bluer::l2cap::{SeqPacket, SeqPacketListener, SocketAddr};
use bluer::{Adapter, AddressType, Session};

/// HumanInterfaceDeviceServiceClass.
const HID_UUID: uuid::Uuid = uuid::uuid!("00001124-0000-1000-8000-00805f9b34fb");

const PSM_CTRL: u16 = 17;
const PSM_INTR: u16 = 19;

/// Boot-protocol keyboard report map, with Report ID 1.
///
/// Kept byte-exact and hand-annotated: the host parses this to decide what our
/// reports mean, and a single wrong byte turns every keystroke into silence.
const REPORT_MAP: &str = concat!(
    "05010906a101", // Usage Page (Generic Desktop), Usage (Keyboard), Collection (Application)
    "8501",         //   Report ID (1)
    "0507",         //   Usage Page (Keyboard/Keypad)
    "19e029e7",     //   Usage Min/Max 224..231 -- the eight modifiers
    "15002501",     //   Logical Min/Max 0..1
    "75019508",     //   Report Size 1 x Count 8
    "8102",         //   Input (Data,Var,Abs)  -> modifier byte
    "95017508",     //   Report Count 1 x Size 8
    "8101",         //   Input (Const)         -> reserved byte
    "95057501",     //   Report Count 5 x Size 1
    "0508",         //   Usage Page (LEDs)
    "19012905",     //   Usage Min/Max 1..5
    "9102",         //   Output (Data,Var,Abs) -> LED report
    "95017503",     //   Report Count 1 x Size 3
    "9101",         //   Output (Const)        -> LED padding
    "95067508",     //   Report Count 6 x Size 8
    "15002565",     //   Logical Min/Max 0..101
    "0507",         //   Usage Page (Keyboard/Keypad)
    "19002965",     //   Usage Min/Max 0..101
    "8100",         //   Input (Data,Array)    -> the six key slots
    "c0",           // End Collection
);

fn sdp_record() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" ?>
<record>
  <attribute id="0x0001"><sequence><uuid value="0x1124" /></sequence></attribute>
  <attribute id="0x0004">
    <sequence>
      <sequence><uuid value="0x0100" /><uint16 value="0x0011" /></sequence>
      <sequence><uuid value="0x0011" /></sequence>
    </sequence>
  </attribute>
  <attribute id="0x0005"><sequence><uuid value="0x1002" /></sequence></attribute>
  <attribute id="0x0006">
    <sequence><uint16 value="0x656e" /><uint16 value="0x006a" /><uint16 value="0x0100" /></sequence>
  </attribute>
  <attribute id="0x0009">
    <sequence><sequence><uuid value="0x1124" /><uint16 value="0x0100" /></sequence></sequence>
  </attribute>
  <attribute id="0x000d">
    <sequence>
      <sequence>
        <sequence><uuid value="0x0100" /><uint16 value="0x0013" /></sequence>
        <sequence><uuid value="0x0011" /></sequence>
      </sequence>
    </sequence>
  </attribute>
  <attribute id="0x0100"><text value="rpi-hub Keyboard" /></attribute>
  <attribute id="0x0101"><text value="USB keyboard bridged over Bluetooth" /></attribute>
  <attribute id="0x0102"><text value="rpi-hub" /></attribute>
  <attribute id="0x0200"><uint16 value="0x0100" /></attribute>
  <attribute id="0x0201"><uint16 value="0x0111" /></attribute>
  <attribute id="0x0202"><uint8 value="0x40" /></attribute>
  <attribute id="0x0203"><uint8 value="0x00" /></attribute>
  <attribute id="0x0204"><boolean value="true" /></attribute>
  <attribute id="0x0205"><boolean value="true" /></attribute>
  <attribute id="0x0206">
    <sequence>
      <sequence>
        <uint8 value="0x22" />
        <text encoding="hex" value="{REPORT_MAP}" />
      </sequence>
    </sequence>
  </attribute>
  <attribute id="0x0207">
    <sequence><sequence><uint16 value="0x0409" /><uint16 value="0x0100" /></sequence></sequence>
  </attribute>
  <attribute id="0x020b"><uint16 value="0x0100" /></attribute>
  <attribute id="0x020c"><uint16 value="0x0c80" /></attribute>
  <attribute id="0x020d"><boolean value="false" /></attribute>
  <attribute id="0x020e"><boolean value="false" /></attribute>
  <attribute id="0x020f"><uint16 value="0x0640" /></attribute>
  <attribute id="0x0210"><uint16 value="0x0320" /></attribute>
</record>
"#
    )
}

/// A live HID link to one host.
pub struct HidLink {
    _ctrl: SeqPacket,
    intr: SeqPacket,
    peer: bluer::Address,
}

impl HidLink {
    pub fn peer(&self) -> bluer::Address {
        self.peer
    }

    /// Push one 10-byte wire report down the interrupt channel.
    pub async fn send(&self, wire_report: &[u8; 10]) -> Result<()> {
        self.intr.send(wire_report).await.context("sending HID report")?;
        Ok(())
    }
}

/// The Bluetooth side of the bridge: advertises as a keyboard and accepts hosts.
pub struct HidPeripheral {
    adapter: Adapter,
    ctrl: SeqPacketListener,
    intr: SeqPacketListener,
    _profile: bluer::rfcomm::ProfileHandle,
}

impl HidPeripheral {
    pub async fn new(alias: &str) -> Result<Self> {
        let session = Session::new().await.context("connecting to BlueZ")?;
        let adapter = session.default_adapter().await.context("no bluetooth adapter")?;

        adapter.set_powered(true).await?;
        adapter.set_alias(alias.to_string()).await?;
        adapter.set_pairable(true).await?;
        adapter.set_discoverable(true).await?;

        // Publish the SDP record. We never read from the handle -- registering
        // the profile is purely how BlueZ is told to advertise HID -- but it
        // must stay alive, so it is held in the struct.
        let profile = bluer::rfcomm::Profile {
            uuid: HID_UUID,
            service_record: Some(sdp_record()),
            role: Some(bluer::rfcomm::Role::Server),
            require_authentication: Some(true),
            require_authorization: Some(false),
            auto_connect: Some(true),
            ..Default::default()
        };
        let profile = session
            .register_profile(profile)
            .await
            .context("registering HID profile (is bluetoothd running with --noplugin=input?)")?;

        // PSMs below 0x1000 are privileged, so this is where a non-root run dies.
        let bind = |psm| SocketAddr::new(bluer::Address::any(), AddressType::BrEdr, psm);
        let ctrl = SeqPacketListener::bind(bind(PSM_CTRL))
            .await
            .context("binding L2CAP PSM 17 (needs root)")?;
        let intr = SeqPacketListener::bind(bind(PSM_INTR))
            .await
            .context("binding L2CAP PSM 19 (needs root)")?;

        Ok(Self { adapter, ctrl, intr, _profile: profile })
    }

    /// Mark a host trusted, so BlueZ stops asking about it.
    ///
    /// An untrusted device makes BlueZ ask an agent for authorisation on every
    /// reconnect, and we run headless with no agent to ask.
    ///
    /// This deliberately takes one address rather than enumerating what is
    /// paired. The paired list on a Pi is a junk drawer -- speakers, phones,
    /// projectors -- and trusting (let alone dialling) all of it is how the
    /// keyboard ends up connected to a projector.
    pub async fn trust(&self, peer: bluer::Address) -> Result<()> {
        let device = self.adapter.device(peer)?;
        if !device.is_paired().await.unwrap_or(false) {
            // Not fatal: the host may simply not have paired yet, and it can
            // still reach us by connecting inbound.
            eprintln!("warning: {peer} is pinned with --host but is not paired yet");
            return Ok(());
        }
        if !device.is_trusted().await.unwrap_or(false) {
            device.set_trusted(true).await.context("marking the host trusted")?;
        }
        Ok(())
    }

    /// Wait for a host to open both HID channels.
    ///
    /// The host connects control first, then interrupt. We accept in that order
    /// rather than concurrently, which is what real keyboards see in practice.
    pub async fn accept(&self) -> Result<HidLink> {
        let (ctrl, ctrl_addr) = self.ctrl.accept().await.context("accepting control channel")?;
        let (intr, intr_addr) = self.intr.accept().await.context("accepting interrupt channel")?;

        if ctrl_addr.addr != intr_addr.addr {
            anyhow::bail!(
                "control channel from {} but interrupt from {}",
                ctrl_addr.addr,
                intr_addr.addr
            );
        }

        Ok(HidLink { _ctrl: ctrl, intr, peer: ctrl_addr.addr })
    }

    /// Dial out to a host we are already bonded with.
    ///
    /// This is the reconnect path: after the Mac sleeps or the Pi reboots, the
    /// host does not necessarily come back to us, so the keyboard has to knock.
    pub async fn connect(&self, peer: bluer::Address) -> Result<HidLink> {
        let target = |psm| SocketAddr::new(peer, AddressType::BrEdr, psm);
        let ctrl = SeqPacket::connect(target(PSM_CTRL)).await.context("dialling control channel")?;
        let intr = SeqPacket::connect(target(PSM_INTR)).await.context("dialling interrupt channel")?;
        Ok(HidLink { _ctrl: ctrl, intr, peer })
    }
}
