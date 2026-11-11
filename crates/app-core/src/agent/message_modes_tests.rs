//! `[messages]` delivery modes: how many queued messages one delivery point
//! carries, and whether a queued message stops the running tools.

use super::*;
use crate::config::{InterruptMode, MessageDeliveryMode, MessagesConfig};
use horon_agent_agent_runtime::{SoftInterruptQueue, SoftInterruptSource};

fn message(content: &str, urgent: bool) -> SoftInterruptMessage {
    SoftInterruptMessage {
        content: content.to_string(),
        images: Vec::new(),
        urgent,
        source: SoftInterruptSource::User,
        client_id: None,
        input_id: None,
    }
}

fn config(follow_up: MessageDeliveryMode, steering: MessageDeliveryMode, interrupt: InterruptMode) -> MessagesConfig {
    MessagesConfig {
        follow_up_mode: follow_up,
        steering_mode: steering,
        interrupt_mode: interrupt,
    }
}

fn queue_with(contents: &[(&str, bool)]) -> SoftInterruptQueue {
    let queue = SoftInterruptQueue::new();
    for (content, urgent) in contents {
        queue.lock().expect("queue lock").push(message(content, *urgent));
    }
    queue
}

#[test]
fn the_shipped_defaults_deliver_everything_and_wait_for_tools() {
    let config = MessagesConfig::default();

    assert_eq!(
        delivery_limit(&config, None),
        usize::MAX,
        "an unconfigured daemon delivers every queued message at once, as it always has"
    );
    assert!(!config.interrupts_running_tools(), "wait is the existing timing");
    assert_eq!(config.follow_up_mode, MessageDeliveryMode::All);
    assert_eq!(config.steering_mode, MessageDeliveryMode::All);
}

#[test]
fn one_at_a_time_caps_the_delivery_at_one_message() {
    let config = config(
        MessageDeliveryMode::OneAtATime,
        MessageDeliveryMode::OneAtATime,
        InterruptMode::Wait,
    );

    assert_eq!(delivery_limit(&config, None), 1);
}

#[test]
fn the_first_delivery_of_a_turn_follows_follow_up_mode() {
    let config = config(
        MessageDeliveryMode::All,
        MessageDeliveryMode::OneAtATime,
        InterruptMode::Wait,
    );
    let queue = queue_with(&[("one", false), ("two", false)]);
    let mut guard = InFlightGuard::new("s1".to_string(), queue.clone());

    assert_eq!(
        delivery_limit(&config, Some(&guard)),
        usize::MAX,
        "leftovers from the previous turn are follow-ups, not steering"
    );

    let moved = guard.transfer(usize::MAX);
    assert_eq!(moved.len(), 2, "follow_up_mode all takes both");
    assert!(queue.pending_snapshot().is_empty());
}

#[test]
fn later_deliveries_in_the_same_turn_follow_steering_mode() {
    let config = config(
        MessageDeliveryMode::All,
        MessageDeliveryMode::OneAtATime,
        InterruptMode::Wait,
    );
    let queue = queue_with(&[("one", false)]);
    let mut guard = InFlightGuard::new("s1".to_string(), queue.clone());

    guard.transfer(delivery_limit(&config, Some(&guard)));
    queue.lock().expect("queue lock").push(message("two", false));
    queue.lock().expect("queue lock").push(message("three", false));

    let limit = delivery_limit(&config, Some(&guard));
    assert_eq!(limit, 1, "steering_mode one-at-a-time takes one message");
    let moved = guard.transfer(limit);
    assert_eq!(
        moved.iter().map(|message| message.content.as_str()).collect::<Vec<_>>(),
        vec!["two"],
        "the oldest leftover goes first"
    );
    assert_eq!(
        queue
            .pending_snapshot()
            .into_iter()
            .map(|message| message.content)
            .collect::<Vec<_>>(),
        vec!["three"],
        "the rest wait for the next delivery point"
    );
}

#[test]
fn a_delivery_that_moves_nothing_keeps_the_follow_up_slot() {
    let config = config(
        MessageDeliveryMode::All,
        MessageDeliveryMode::OneAtATime,
        InterruptMode::Wait,
    );
    let queue = SoftInterruptQueue::new();
    let mut guard = InFlightGuard::new("s1".to_string(), queue);

    assert!(guard.transfer(usize::MAX).is_empty());
    assert!(
        guard.takes_follow_ups(),
        "an empty delivery must not consume the follow-up slot of the turn"
    );
    assert_eq!(delivery_limit(&config, Some(&guard)), usize::MAX);
}

#[test]
fn an_urgent_message_skips_the_running_tools_even_in_wait_mode() {
    let queue = queue_with(&[("stop now", true)]);

    assert!(urgent_interrupt_pending_in(&queue, false));
}

#[test]
fn a_plain_message_waits_for_the_running_tool_in_wait_mode() {
    let queue = queue_with(&[("no rush", false)]);

    assert!(
        !urgent_interrupt_pending_in(&queue, false),
        "wait mode keeps the per-message flag: nothing is urgent here"
    );
}

#[test]
fn immediate_mode_promotes_a_plain_message_to_urgent() {
    let queue = queue_with(&[("no rush", false)]);

    assert!(
        urgent_interrupt_pending_in(&queue, true),
        "immediate mode stops the tools for any queued message"
    );
}

#[test]
fn immediate_mode_does_nothing_with_an_empty_queue() {
    let queue = SoftInterruptQueue::new();

    assert!(!urgent_interrupt_pending_in(&queue, true));
}
