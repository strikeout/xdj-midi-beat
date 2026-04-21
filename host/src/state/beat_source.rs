//! Beat source tag.

/// Which data source last updated the master state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeatSource {
    /// Pioneer Pro DJ Link (hardware CDJs/XDJs on Ethernet).
    ProLink,
    /// Ableton Link (rekordbox Performance mode or other Link peers).
    AbletonLink,
}
