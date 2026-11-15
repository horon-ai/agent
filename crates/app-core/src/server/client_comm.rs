pub(super) use super::client_comm_message::handle_comm_message;
pub(super) use super::client_comm_swarms::handle_comm_list;

#[cfg(test)]
#[path = "client_comm_tests.rs"]
mod client_comm_tests;
