use super::*;

#[test]
fn test_model_picker_cursor_models_have_cursor_route() {
    let mut app = create_test_app();
    configure_test_remote_models_with_cursor(&mut app);

    app.open_model_picker();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker should be open");

    let composer_entry = picker
        .entries
        .iter()
        .find(|m| m.name == "composer-2-fast")
        .expect("composer-2-fast should be in picker");

    assert!(
        composer_entry
            .options
            .iter()
            .any(|r| r.api_method == "cursor"),
        "composer-2-fast should have a cursor route, got: {:?}",
        composer_entry.options
    );
}
