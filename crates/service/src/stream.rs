//! Re-export of framed protobuf control streams from `veloxdesk-transport`.

pub use veloxdesk_transport::{ControlReceiver as EnvelopeReader, ControlSender as EnvelopeWriter, framed};
