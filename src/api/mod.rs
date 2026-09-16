//! HTTP control boundary. Commands are executed by the bot's game loop.

mod replies;
mod request;
mod response;
mod routes;
mod server;
mod telemetry;

pub(crate) use server::run_api_server;
