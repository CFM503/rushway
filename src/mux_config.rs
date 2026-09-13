pub const DEFAULT_MUX_SESSIONS: usize = 4;
pub const MAX_MUX_SESSIONS: usize = 64;
pub const MAX_MUX_STREAMS_PER_SESSION: usize = 256;

pub fn clamp_mux_sessions(value: usize) -> usize {
    value.clamp(1, MAX_MUX_SESSIONS)
}
