use splice_pty::{
    flow::CreditWindow, PtyError, PtySession, PtySessionContract, PtySessionEvent,
    PtySessionLifecycle,
};

fn requires_platform_neutral_contract<T: PtySessionContract>() {}

#[test]
fn session_contract_keeps_identity_and_early_output_attributed() {
    requires_platform_neutral_contract::<PtySession>();

    let early_output = PtySessionEvent::from_output(41, "ready before listener".to_owned());
    let natural_exit = PtySessionEvent::natural_exit(41);

    assert_eq!(early_output.session_id(), 41);
    assert_eq!(early_output.output(), Some("ready before listener"));
    assert_eq!(natural_exit.session_id(), 41);
    assert_eq!(natural_exit.output(), None);
}

#[test]
fn session_contract_distinguishes_natural_exit_from_idempotent_close() {
    let lifecycle = PtySessionLifecycle::new(7);

    assert_eq!(lifecycle.id(), 7);
    assert!(lifecycle.should_emit_natural_exit());
    assert!(lifecycle.begin_close());
    assert!(!lifecycle.begin_close());
    assert!(!lifecycle.should_emit_natural_exit());
}

#[test]
fn session_contract_preserves_input_error_and_ack_backpressure_boundaries() {
    let credit = CreditWindow::new(4);
    credit.consume(4);
    credit.replenish(2);
    credit.replenish(99);

    assert_eq!(credit.available(), 4, "stale ACKs must not exceed capacity");
    assert!(PtyError::SessionClosed.is_terminal_closed());
}
