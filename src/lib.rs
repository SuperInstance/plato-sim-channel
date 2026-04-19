//! plato-sim-channel — ChannelLayer adapter for plato-sim-bridge
//!
//! Bridges simulation ↔ live environments through the Ship Interconnection
//! Protocol's ChannelLayer trait. Messages can be sent to simulation channels
//! or live channels, with automatic mode switching.
//!
//! Sprint 3 Task S3-4: implement ChannelLayer for plato-sim-bridge.

use std::collections::{HashMap, VecDeque};

// ── Channel Trait ────────────────────────────────────────

/// Channel layer: simulation ↔ live bridging.
/// Matches plato-ship-protocol::ChannelLayer exactly.
pub trait ChannelLayer {
    fn bridge_send(&mut self, channel: u8, msg: &[u8]) -> bool;
    fn bridge_recv(&mut self, channel: u8) -> Option<Vec<u8>>;
    fn is_live(&self) -> bool;
}

// ── Channel Types ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Connected to live production environment
    Live,
    /// Running in simulation mode
    Simulated,
    /// Bridging: sim output feeds into live input
    Bridging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ChannelKind {
    /// Fleet coordination channel
    Fleet = 0,
    /// Training data channel
    Training = 1,
    /// Event/notification channel
    Event = 2,
    /// Tile sharing channel
    Tiles = 3,
    /// Trust signal channel
    Trust = 4,
    /// Custom/reserved channels 5-255
    Custom(u8),
}

impl ChannelKind {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => ChannelKind::Fleet,
            1 => ChannelKind::Training,
            2 => ChannelKind::Event,
            3 => ChannelKind::Tiles,
            4 => ChannelKind::Trust,
            n => ChannelKind::Custom(n),
        }
    }

    pub fn to_byte(self) -> u8 {
        match self {
            ChannelKind::Fleet => 0,
            ChannelKind::Training => 1,
            ChannelKind::Event => 2,
            ChannelKind::Tiles => 3,
            ChannelKind::Trust => 4,
            ChannelKind::Custom(n) => n,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ChannelKind::Fleet => "fleet",
            ChannelKind::Training => "training",
            ChannelKind::Event => "event",
            ChannelKind::Tiles => "tiles",
            ChannelKind::Trust => "trust",
            ChannelKind::Custom(_) => "custom",
        }
    }
}

// ── Channel Message ──────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub payload: Vec<u8>,
    pub channel: ChannelKind,
    pub source: String,      // agent or system that sent it
    pub timestamp: u64,      // nanosecond
    pub sim_origin: bool,    // true if this came from simulation
    pub quality_score: f32,  // 0.0-1.0, how good is this data
}

impl ChannelMessage {
    pub fn new(channel: ChannelKind, source: &str, payload: &[u8]) -> Self {
        Self {
            payload: payload.to_vec(),
            channel,
            source: source.to_string(),
            timestamp: nanos_now(),
            sim_origin: false,
            quality_score: 0.5,
        }
    }

    pub fn from_sim(channel: ChannelKind, source: &str, payload: &[u8]) -> Self {
        let mut msg = Self::new(channel, source, payload);
        msg.sim_origin = true;
        msg
    }

    pub fn with_quality(mut self, q: f32) -> Self {
        self.quality_score = q.max(0.0).min(1.0);
        self
    }
}

// ── Channel Adapter ──────────────────────────────────────

/// Bridges simulation and live environments through typed channels.
#[derive(Debug, Clone)]
pub struct ChannelAdapter {
    mode: ChannelMode,
    channels: HashMap<u8, VecDeque<ChannelMessage>>,
    max_buffer: usize,
    messages_sent: u64,
    messages_received: u64,
    messages_bridged: u64,
}

impl ChannelAdapter {
    pub fn new(mode: ChannelMode) -> Self {
        Self {
            mode,
            channels: HashMap::new(),
            max_buffer: 256,
            messages_sent: 0,
            messages_received: 0,
            messages_bridged: 0,
        }
    }

    pub fn live() -> Self { Self::new(ChannelMode::Live) }
    pub fn simulated() -> Self { Self::new(ChannelMode::Simulated) }
    pub fn bridging() -> Self { Self::new(ChannelMode::Bridging) }

    /// Set maximum buffer per channel
    pub fn with_max_buffer(mut self, max: usize) -> Self {
        self.max_buffer = max;
        self
    }

    /// Send a typed message to a channel
    pub fn send_typed(&mut self, kind: ChannelKind, source: &str, payload: &[u8]) -> bool {
        let ch = kind.to_byte();
        let cap = self.max_buffer;
        let buf = self.channels.entry(ch).or_insert_with(VecDeque::new);
        if buf.len() >= cap { return false; }
        buf.push_back(ChannelMessage::new(kind, source, payload));
        self.messages_sent += 1;
        true
    }

    /// Send a simulation-originated message
    pub fn send_sim(&mut self, kind: ChannelKind, source: &str, payload: &[u8]) -> bool {
        let ch = kind.to_byte();
        let cap = self.max_buffer;
        let buf = self.channels.entry(ch).or_insert_with(VecDeque::new);
        if buf.len() >= cap { return false; }
        buf.push_back(ChannelMessage::from_sim(kind, source, payload));
        self.messages_sent += 1;
        self.messages_bridged += 1;
        true
    }

    /// Receive from a channel (returns payload only)
    pub fn recv(&mut self, kind: ChannelKind) -> Option<Vec<u8>> {
        let ch = kind.to_byte();
        let msg = self.channels.get_mut(&ch)?.pop_front()?;
        self.messages_received += 1;
        Some(msg.payload)
    }

    /// Receive full message with metadata
    pub fn recv_full(&mut self, kind: ChannelKind) -> Option<ChannelMessage> {
        let ch = kind.to_byte();
        let msg = self.channels.get_mut(&ch)?.pop_front()?;
        self.messages_received += 1;
        Some(msg)
    }

    /// Bridge: take all sim messages from one channel, tag them, push to another
    pub fn bridge(&mut self, from: ChannelKind, to: ChannelKind) -> usize {
        let from_ch = from.to_byte();
        // Take all messages from source channel
        let all_msgs: Vec<ChannelMessage> = self.channels.remove(&from_ch)
            .unwrap_or_default().into_iter().collect();
        
        // Separate sim from non-sim
        let mut sim = Vec::new();
        let mut non_sim = Vec::new();
        for msg in all_msgs {
            if msg.sim_origin { sim.push(msg); } else { non_sim.push(msg); }
        }
        
        // Put non-sim messages back
        if !non_sim.is_empty() {
            self.channels.insert(from_ch, non_sim.into_iter().collect());
        }
        
        // Move sim messages to target channel
        let count = sim.len();
        let target = self.channels.entry(to.to_byte()).or_insert_with(VecDeque::new);
        for msg in sim {
            if target.len() < self.max_buffer {
                target.push_back(msg);
                self.messages_bridged += 1;
            }
        }
        count
    }

    /// Buffer size for a channel
    pub fn channel_size(&self, kind: ChannelKind) -> usize {
        self.channels.get(&kind.to_byte()).map(|b| b.len()).unwrap_or(0)
    }

    /// Total buffered across all channels
    pub fn total_buffered(&self) -> usize {
        self.channels.values().map(|b| b.len()).sum()
    }

    /// Switch mode
    pub fn set_mode(&mut self, mode: ChannelMode) {
        self.mode = mode;
    }

    /// Stats
    pub fn stats(&self) -> ChannelStats {
        ChannelStats {
            mode: self.mode,
            messages_sent: self.messages_sent,
            messages_received: self.messages_received,
            messages_bridged: self.messages_bridged,
            active_channels: self.channels.len(),
            total_buffered: self.total_buffered(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ChannelStats {
    pub mode: ChannelMode,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub messages_bridged: u64,
    pub active_channels: usize,
    pub total_buffered: usize,
}

impl ChannelLayer for ChannelAdapter {
    fn bridge_send(&mut self, channel: u8, msg: &[u8]) -> bool {
        let is_sim = self.mode != ChannelMode::Live;
        let kind = ChannelKind::from_byte(channel);
        let buf = self.channels.entry(channel).or_insert_with(VecDeque::new);
        if buf.len() >= self.max_buffer { return false; }
        let mut ch_msg = ChannelMessage::new(kind, "bridge", msg);
        ch_msg.sim_origin = is_sim;
        buf.push_back(ch_msg);
        self.messages_sent += 1;
        true
    }

    fn bridge_recv(&mut self, channel: u8) -> Option<Vec<u8>> {
        let msg = self.channels.get_mut(&channel)?.pop_front()?;
        self.messages_received += 1;
        Some(msg.payload)
    }

    fn is_live(&self) -> bool {
        self.mode == ChannelMode::Live
    }
}

fn nanos_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_send_recv() {
        let mut ch = ChannelAdapter::live();
        ch.send_typed(ChannelKind::Fleet, "oracle1", b"hello");
        let msg = ch.recv(ChannelKind::Fleet).unwrap();
        assert_eq!(msg, b"hello");
    }

    #[test]
    fn test_channel_recv_empty() {
        let mut ch = ChannelAdapter::simulated();
        assert!(ch.recv(ChannelKind::Event).is_none());
    }

    #[test]
    fn test_channel_ordering() {
        let mut ch = ChannelAdapter::live();
        ch.send_typed(ChannelKind::Training, "a", b"1");
        ch.send_typed(ChannelKind::Training, "b", b"2");
        ch.send_typed(ChannelKind::Training, "c", b"3");

        assert_eq!(ch.recv(ChannelKind::Training).unwrap(), b"1");
        assert_eq!(ch.recv(ChannelKind::Training).unwrap(), b"2");
    }

    #[test]
    fn test_sim_messages() {
        let mut ch = ChannelAdapter::simulated();
        ch.send_sim(ChannelKind::Tiles, "sim", b"tile_data");
        let msg = ch.recv_full(ChannelKind::Tiles).unwrap();
        assert!(msg.sim_origin);
        assert_eq!(msg.payload, b"tile_data");
    }

    #[test]
    fn test_bridge() {
        let mut ch = ChannelAdapter::bridging();
        // Send sim messages to fleet channel
        ch.send_sim(ChannelKind::Fleet, "sim", b"sim_msg_1");
        ch.send_sim(ChannelKind::Fleet, "sim", b"sim_msg_2");
        // Send a live message (should NOT be bridged)
        ch.send_typed(ChannelKind::Fleet, "live_agent", b"live_msg");

        // Bridge fleet → event
        let bridged = ch.bridge(ChannelKind::Fleet, ChannelKind::Event);
        assert_eq!(bridged, 2); // only sim messages

        // Event channel should have the 2 sim messages
        assert_eq!(ch.channel_size(ChannelKind::Event), 2);
        // Fleet channel should still have the live message
        assert_eq!(ch.channel_size(ChannelKind::Fleet), 1);
    }

    #[test]
    fn test_capacity_limit() {
        let mut ch = ChannelAdapter::live().with_max_buffer(2);
        assert!(ch.send_typed(ChannelKind::Event, "a", b"1"));
        assert!(ch.send_typed(ChannelKind::Event, "a", b"2"));
        assert!(!ch.send_typed(ChannelKind::Event, "a", b"3")); // over capacity
    }

    #[test]
    fn test_is_live() {
        assert!(ChannelAdapter::live().is_live());
        assert!(!ChannelAdapter::simulated().is_live());
        assert!(!ChannelAdapter::bridging().is_live());
    }

    #[test]
    fn test_channel_layer_trait() {
        let mut ch = ChannelAdapter::live();
        assert!(ch.bridge_send(0, b"fleet_msg")); // channel 0 = fleet
        assert!(ch.bridge_send(3, b"tile_msg")); // channel 3 = tiles

        let fleet = ch.bridge_recv(0).unwrap();
        assert_eq!(fleet, b"fleet_msg");

        let tiles = ch.bridge_recv(3).unwrap();
        assert_eq!(tiles, b"tile_msg");
    }

    #[test]
    fn test_channel_kinds() {
        assert_eq!(ChannelKind::from_byte(0), ChannelKind::Fleet);
        assert_eq!(ChannelKind::from_byte(1), ChannelKind::Training);
        assert_eq!(ChannelKind::from_byte(5), ChannelKind::Custom(5));
        assert_eq!(ChannelKind::Fleet.to_byte(), 0);
        assert_eq!(ChannelKind::Custom(99).to_byte(), 99);
    }

    #[test]
    fn test_quality_score() {
        let msg = ChannelMessage::new(ChannelKind::Event, "src", b"data").with_quality(0.95);
        assert_eq!(msg.quality_score, 0.95);
    }

    #[test]
    fn test_quality_clamping() {
        let msg = ChannelMessage::new(ChannelKind::Event, "src", b"data").with_quality(1.5);
        assert_eq!(msg.quality_score, 1.0);
    }

    #[test]
    fn test_stats() {
        let mut ch = ChannelAdapter::bridging();
        ch.send_typed(ChannelKind::Fleet, "a", b"1");
        ch.send_sim(ChannelKind::Tiles, "sim", b"2");
        ch.recv(ChannelKind::Fleet);

        let stats = ch.stats();
        assert_eq!(stats.messages_sent, 2);
        assert_eq!(stats.messages_received, 1);
        assert_eq!(stats.messages_bridged, 1);
        assert_eq!(stats.active_channels, 2);
    }

    #[test]
    fn test_mode_switch() {
        let mut ch = ChannelAdapter::simulated();
        assert!(!ch.is_live());
        ch.set_mode(ChannelMode::Live);
        assert!(ch.is_live());
    }

    #[test]
    fn test_multiple_channels_independent() {
        let mut ch = ChannelAdapter::live();
        ch.send_typed(ChannelKind::Fleet, "a", b"f1");
        ch.send_typed(ChannelKind::Training, "a", b"t1");

        assert_eq!(ch.recv(ChannelKind::Fleet).unwrap(), b"f1");
        assert_eq!(ch.recv(ChannelKind::Training).unwrap(), b"t1");
        assert!(ch.recv(ChannelKind::Event).is_none()); // empty channel
    }

    #[test]
    fn test_bridge_empty() {
        let mut ch = ChannelAdapter::bridging();
        let bridged = ch.bridge(ChannelKind::Event, ChannelKind::Fleet);
        assert_eq!(bridged, 0);
    }
}
