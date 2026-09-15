//! Luanti network protocol: transport, command encoding/decoding, and authentication.

mod connection;
mod events;
mod inbound;
mod outbound;
pub(crate) mod protocol;
mod srp;
mod wire;

pub(crate) use connection::MtpConnection;
pub(crate) use events::{
    ActiveObjectInit, ActiveObjectMessage, InventoryLocation, MtpEvent, TracePacket,
};
pub(crate) use srp::SrpClient;

use protocol::{AUTH_MECHANISM_FIRST_SRP, AUTH_MECHANISM_SRP};

pub(crate) fn auth_mechanism_supported(auth_mechs: u32) -> bool {
    auth_mechs & AUTH_MECHANISM_FIRST_SRP != 0 || auth_mechs & AUTH_MECHANISM_SRP != 0
}

pub(crate) fn auth_mechanism_choice(auth_mechs: u32) -> AuthChoice {
    if auth_mechs & AUTH_MECHANISM_FIRST_SRP != 0 {
        AuthChoice::FirstSrp
    } else {
        AuthChoice::Srp
    }
}

pub(crate) enum AuthChoice {
    FirstSrp,
    Srp,
}

pub(crate) fn should_send_client_ready(got_itemdef: bool, got_nodedef: bool) -> bool {
    got_itemdef && got_nodedef
}
