//! Gettext-backed translation boundary for GTK-visible copy.

use gtk4::glib;

const DOMAIN: &str = "noire";

/// Resolves one source-language message from the system locale catalog.
pub(crate) fn tr(message: &str) -> glib::GString {
    glib::dgettext(Some(DOMAIN), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untranslated_messages_retain_the_source_language() {
        assert_eq!(tr("Noire diagnostics"), "Noire diagnostics");
    }
}
