//! Shared connection setup and authentication for all command modes.

use anyhow::{bail, Context, Result};
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use crate::network::{auth_mechanism_choice, auth_mechanism_supported, AuthChoice, MtpConnection, SrpClient};

pub(crate) fn open_connection(address: &str, read_timeout: Duration) -> Result<MtpConnection> {
    let addr: SocketAddr = address
        .parse()
        .with_context(|| format!("invalid address: {address}"))?;
    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    socket
        .set_read_timeout(Some(read_timeout))
        .context("set read timeout")?;

    let mut connection = MtpConnection::new(socket, addr);
    connection.send_dummy_reliable()?;
    Ok(connection)
}

pub(crate) fn start_authentication(
    connection: &mut MtpConnection,
    player: &str,
    password: &str,
    auth_mechanisms: u32,
    srp: &mut Option<SrpClient>,
    verbose: bool,
) -> Result<()> {
    if !auth_mechanism_supported(auth_mechanisms) {
        bail!("unsupported auth mechanisms: 0x{auth_mechanisms:08x}");
    }
    match auth_mechanism_choice(auth_mechanisms) {
        AuthChoice::FirstSrp => {
            if verbose {
                println!("auth: FIRST_SRP register");
            }
            connection.send_first_srp(player, password)?;
        }
        AuthChoice::Srp => {
            if verbose {
                println!("auth: SRP login");
            }
            let client = SrpClient::new(player, password)?;
            connection.send_srp_a(&client.a_bytes)?;
            *srp = Some(client);
        }
    }
    Ok(())
}

pub(crate) fn answer_srp_challenge(
    connection: &mut MtpConnection,
    srp: Option<&SrpClient>,
    salt: &[u8],
    server_public_key: &[u8],
    verbose: bool,
) -> Result<()> {
    if let Some(client) = srp {
        if verbose {
            println!("auth: got SRP S,B; sending M");
        }
        let proof = client.process_challenge(salt, server_public_key)?;
        connection.send_srp_m(&proof)?;
    }
    Ok(())
}
