//! Track-change notification.

/// Emitted when a device loads a different track so the metadata fetcher can
/// query the dbserver for title/artist/key.
#[derive(Debug, Clone)]
pub struct TrackChange {
    /// The CDJ that changed track.
    pub device_number: u8,
    /// Device the track was loaded from (Dr).
    pub track_source_player: u8,
    /// Media slot (Sr).
    pub track_slot: u8,
    /// Track type (Tr).
    pub track_type: u8,
    /// Rekordbox database ID.
    pub rekordbox_id: u32,
}
