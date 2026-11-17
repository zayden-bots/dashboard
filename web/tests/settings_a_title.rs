use web::app::guild::guild_id::settings::title;
use web::nav::section;

#[test]
fn section_titles_name_the_page() {
    for (slug, expected) in [
        ("general", "Server settings - Zayden Dashboard"),
        ("ai", "AI Chat - Zayden Dashboard"),
        ("deadlock", "Deadlock - Zayden Dashboard"),
        ("family", "Family - Zayden Dashboard"),
        ("honeypot", "Honeypot - Zayden Dashboard"),
        ("lfg", "LFG - Zayden Dashboard"),
        ("music", "Music - Zayden Dashboard"),
        ("patreon", "Patreon - Zayden Dashboard"),
        ("support", "Support - Zayden Dashboard"),
        ("temp-voice", "Temp voice - Zayden Dashboard"),
        ("youtube", "YouTube - Zayden Dashboard"),
    ] {
        assert_eq!(title(section(slug)), expected, "{slug}");
    }
}

#[test]
fn missing_and_unknown_sections_title_as_server_settings() {
    for slug in ["", "bogus", "greetings", "levels", "reaction-roles", "General"] {
        assert_eq!(
            title(section(slug)),
            "Server settings - Zayden Dashboard",
            "{slug}"
        );
    }
}
