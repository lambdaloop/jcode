// Ctrl+G is an Esc alias (pi / emacs keyboard-quit habit).

#[test]
fn test_ctrl_g_interrupts_running_turn_like_esc() {
    let mut app = create_test_app();
    app.is_processing = true;

    app.handle_key(KeyCode::Char('g'), KeyModifiers::CONTROL)
        .unwrap();

    assert!(app.cancel_requested, "Ctrl+G must interrupt like Esc");
}

#[test]
fn test_ctrl_g_clears_input_when_idle() {
    let mut app = create_test_app();
    app.input = "draft".to_string();
    app.cursor_pos = app.input.len();

    app.handle_key(KeyCode::Char('g'), KeyModifiers::CONTROL)
        .unwrap();

    assert!(app.input.is_empty(), "idle Ctrl+G clears the composer like Esc");
    assert!(!app.cancel_requested);
}

#[test]
fn test_ctrl_g_is_not_bound_to_scroll_bookmark_by_default() {
    let mut app = create_test_app();
    assert!(
        !app.scroll_keys
            .is_bookmark(KeyCode::Char('g'), KeyModifiers::CONTROL),
        "the scroll bookmark is unbound by default so Ctrl+G can interrupt"
    );
    app.handle_key(KeyCode::Char('g'), KeyModifiers::CONTROL)
        .unwrap();
}

#[test]
fn test_readline_ctrl_keys_still_edit_input() {
    let mut app = create_test_app();
    app.input = "hello world".to_string();
    app.cursor_pos = app.input.len();

    app.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.cursor_pos, 0, "Ctrl+A moves to line start");
    app.handle_key(KeyCode::Char('e'), KeyModifiers::CONTROL)
        .unwrap();
    assert_eq!(app.cursor_pos, app.input.len(), "Ctrl+E moves to line end");
    app.handle_key(KeyCode::Char('u'), KeyModifiers::CONTROL)
        .unwrap();
    assert!(app.input.is_empty(), "Ctrl+U kills to line start");
}
