#[test]
fn ci_negative_control() {
    assert!(
        std::env::var_os("MUD_CI_FORCE_TEST_FAILURE").is_none(),
        "intentional CI failure injection"
    );
}
