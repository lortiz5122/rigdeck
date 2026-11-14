//! Donation links shown in the sidebar's "Support RigDeck" section.

use eframe::egui;

/// PayPal Donate page for the author's account (PayPal merchant ID), where the donor types the amount.
pub const PAYPAL_URL: &str = "https://www.paypal.com/donate/?business=7TR6E8QGX7YMJ&currency_code=USD&item_name=RigDeck%20development";
/// PayPal account, shown as text for donors who prefer to send from the PayPal app.
pub const PAYPAL_ACCOUNT: &str = "ortizl@gmail.com";
/// Cash App $Cashtag.
pub const CASHTAG: &str = "$lortiz1721";

/// Cash App's public payment page for the $Cashtag.
pub fn cash_app_url() -> String {
    format!("https://cash.app/{CASHTAG}")
}

pub const WHY: &str = "RigDeck is free and open source (MIT licence), with no ads, no account and no subscription. \
It is built by K1ORT for the ham radio community.";

pub const WHAT_IT_PAYS_FOR: &str = "Donations pay for further development of the app: support for more radios, \
new features, fixes, and the time spent testing on real radios. The code stays free for every amateur.";

pub const THANKS: &str = "Any amount helps. Thank you, and 73 de K1ORT!";

/// The developer's amateur radio call sign.
pub const DEVELOPER_CALL: &str = "K1ORT";

pub fn section(ui: &mut egui::Ui) {
    ui.label(WHY);
    ui.label(WHAT_IT_PAYS_FOR);
    ui.horizontal(|ui| {
        if ui.button("Donate with PayPal").on_hover_text(PAYPAL_URL).clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(PAYPAL_URL));
        }
        let cash = cash_app_url();
        if ui.button("Donate with Cash App").on_hover_text(&cash).clicked() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(cash));
        }
    });
    let weak = ui.visuals().weak_text_color();
    let small = |t: String| egui::RichText::new(t).size(11.0).color(weak);
    ui.label(small(format!("Give any amount you like. PayPal: {PAYPAL_ACCOUNT} · Cash App: {CASHTAG}")));
    ui.label(THANKS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn donation_links_point_at_paypal_and_cash_app() {
        assert_eq!(cash_app_url(), "https://cash.app/$lortiz1721");
        // The Donate page lets the donor choose the amount; a fixed-amount link must not come back.
        assert!(PAYPAL_URL.starts_with("https://www.paypal.com/donate/?business="));
        assert!(!PAYPAL_URL.contains("amount="));
    }
}
