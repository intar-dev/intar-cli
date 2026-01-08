pub const AGENT_X86_64: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/intar-agent-x86_64"));
pub const AGENT_AARCH64: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/intar-agent-aarch64"));

pub fn is_placeholder(binary: &[u8]) -> bool {
    binary == b"PLACEHOLDER_AGENT_BINARY"
}
