use super::*;

#[test]
fn token_usage_totals_counts_cache_reported_inputs_only_when_cache_fields_exist() {
    let mut session = Session::create_with_id(
        "session_token_usage_totals_test".to_string(),
        None,
        Some("Token totals".to_string()),
    );
    session.add_message_ext(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }],
        None,
        Some(StoredTokenUsage {
            prompt_tokens: None,
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
        }),
    );
    session.add_message_ext(
        Role::Assistant,
        vec![ContentBlock::Text {
            text: "second".to_string(),
            cache_control: None,
        }],
        None,
        Some(StoredTokenUsage {
            prompt_tokens: None,
            input_tokens: 200,
            output_tokens: 20,
            cache_read_input_tokens: Some(150),
            cache_creation_input_tokens: Some(25),
        }),
    );

    let totals = session.token_usage_totals();
    assert_eq!(totals.messages_with_token_usage, 2);
    assert_eq!(totals.input_tokens, 300);
    assert_eq!(totals.output_tokens, 30);
    assert_eq!(totals.cache_reported_input_tokens, 200);
    assert_eq!(totals.cache_read_input_tokens, 150);
    assert_eq!(totals.cache_creation_input_tokens, 25);
}

